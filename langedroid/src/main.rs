//! langedroid: orquestrador do container Android do LangeOS (Blueprint §4A, §5A). Zero dependências.
//!
//!   langedroid daemon [--idle-freeze SEG] [--rootfs DIR] [--payload CMD...]   serviço (root, sob lange-init)
//!     --rootfs DIR: raiz Android montada (system + vendor + data); payload padrão vira /init
//!     --allow-32bit: mantém ABIs 32-bit da imagem (padrão: Android só 64-bit, ver abi64_only)
//!     --wayland-dir DIR: socket do compositor do host (ex. /run/lange) visível em /run/user/1000
//!     --prop K=V: sobrepõe propriedade no vendor/build.prop (repetível)
//!     --debug-logcat: logcat do Android inteiro em /dev/kmsg (console do host). Só p/ depuração.
//!   langedroid ctl <open|close|freeze|thaw|status|stop>         cliente
//!   langedroid selftest                                          teste ponta a ponta contra o daemon
//!
//! Container = payload em namespaces novos (pid, mount, uts, ipc, net) dentro de /sys/fs/cgroup/android.
//! Ciclo (perfil Ultra-Light): `open` faz cold start ou descongela; `close` congela após SEG ocioso.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

const SOCK: &str = "/run/langedroid.sock";
const CG: &str = "/sys/fs/cgroup/android";

unsafe extern "C" {
    fn unshare(flags: i32) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(code: i32) -> !;
    fn mount(src: *const u8, target: *const u8, fstype: *const u8, flags: usize, data: *const u8) -> i32;
    fn sethostname(name: *const u8, len: usize) -> i32;
    fn close(fd: i32) -> i32;
    fn umask(mask: u32) -> u32;
    fn getsockopt(fd: i32, level: i32, name: i32, val: *mut u8, len: *mut u32) -> i32;
    fn umount2(target: *const u8, flags: i32) -> i32;
    fn chdir(path: *const u8) -> i32;
    fn syscall(n: i64, ...) -> i64;
    fn mknod(path: *const u8, mode: u32, dev: u64) -> i32;
}
const CLONE_NEWNS: i32 = 0x0002_0000;
const CLONE_NEWUTS: i32 = 0x0400_0000;
const CLONE_NEWIPC: i32 = 0x0800_0000;
const CLONE_NEWPID: i32 = 0x2000_0000;
const CLONE_NEWNET: i32 = 0x4000_0000;
const MS_REC: usize = 0x4000;
const MS_PRIVATE: usize = 0x4_0000;
const MS_BIND: usize = 0x1000;
const MS_RDONLY: usize = 0x1;
const MS_NODEV: usize = 0x4;
const MS_REMOUNT: usize = 0x20;
const MS_NOSUID: usize = 0x2;
const MS_NOEXEC: usize = 0x8;
const MNT_DETACH: i32 = 2;
#[cfg(target_arch = "aarch64")]
const SYS_PIVOT_ROOT: i64 = 41;
#[cfg(target_arch = "x86_64")]
const SYS_PIVOT_ROOT: i64 = 155;

fn cstr(s: &str) -> std::ffi::CString { std::ffi::CString::new(s).unwrap() }

fn mnt(src: &str, tgt: &str, fstype: &str, flags: usize, data: &str) -> std::io::Result<()> {
    let (src, tgt, fstype, data) = (cstr(src), cstr(tgt), cstr(fstype), cstr(data));
    let data_ptr = if data.is_empty() { std::ptr::null() } else { data.as_ptr().cast() };
    if unsafe { mount(src.as_ptr().cast(), tgt.as_ptr().cast(), fstype.as_ptr().cast(), flags, data_ptr) } != 0 {
        let e = std::io::Error::last_os_error();
        return Err(std::io::Error::new(e.kind(), format!("mount {tgt:?}: {e}")));
    }
    Ok(())
}

/// Nós de /dev que o container recebe do host (bind). Nada de bloco/GPU/modem: isolamento.
/// kmsg: logs do init Android aparecem no console do host (debug da etapa 6).
const DEV_NODES: [&str; 7] = ["null", "zero", "full", "random", "urandom", "tty", "kmsg"];

/// Política 64-bit only: remove ABIs 32-bit dos build.prop. Junto com zygote64_only, elimina o
/// zygote32 residente; núcleos ARMv9 nem executam AArch32. Aplicada em cópias (imagem intacta).
fn abi64_only(build_prop: &str) -> String {
    let mut out = String::new();
    for line in build_prop.lines() {
        let key = line.split_once('=').map(|(k, _)| k.trim());
        match key {
            Some(k) if k.ends_with("cpu.abilist32") => continue,
            Some(k) if k.ends_with("cpu.abilist") => { out.push_str(k); out.push_str("=arm64-v8a"); }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// init.zygote64_32.rc sem o serviço zygote_secondary (app_process32). A imagem não traz
/// init.zygote64.rc, então ro.zygote continua zygote64_32 e só o .rc é sobreposto.
fn zygote64_only(rc: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for line in rc.lines() {
        if line.starts_with("service ") { skip = line.starts_with("service zygote_secondary "); }
        if !skip { out.push_str(line); out.push('\n'); }
    }
    out
}

/// Anexa um serviço logcat (stdout -> kmsg via stdio_to_kmsg) a um .rc existente (o init só lê .rc de diretórios read-only).
/// seclabel su: build userdebug; sem domínio o init recusa o serviço.
fn add_debug_logcat(rc: &str) -> String {
    format!("{rc}\n# langedroid --debug-logcat\nservice lange_logcat /system/bin/logcat -b all -v brief\n    user root\n    seclabel u:r:su:s0\n    stdio_to_kmsg\n\non early-init\n    start lange_logcat\n")
}

/// Arquivos do rootfs a sobrepor (bind) com a política 64-bit. Escreve cópias em /run/langedroid.
/// `chave=valor` sobrescreve linha existente ou é anexado ao fim.
fn set_props(build_prop: &str, props: &[(String, String)]) -> String {
    let mut out = String::new();
    for line in build_prop.lines() {
        let key = line.split_once('=').map(|(k, _)| k.trim());
        if key.is_some_and(|k| props.iter().any(|(pk, _)| pk == k)) { continue; }
        out.push_str(line);
        out.push('\n');
    }
    for (k, v) in props { out.push_str(&format!("{k}={v}\n")); }
    out
}

/// Arquivos do rootfs a sobrepor (bind). Transformações no mesmo arquivo são encadeadas.
/// Cópias ficam em /run/langedroid; a imagem Android fica intacta.
fn prop_overrides(root: &str, abi64: bool, debug_logcat: bool, props: &[(String, String)]) -> std::io::Result<Vec<(String, String)>> {
    let dir = "/run/langedroid";
    fs::create_dir_all(dir)?;
    type Policy<'a> = Box<dyn Fn(&str) -> String + 'a>;
    let mut steps: Vec<(&str, Policy)> = vec![];
    if abi64 {
        for f in ["system/build.prop", "vendor/build.prop", "vendor/odm/etc/build.prop"] { // odm > vendor nas ABIs
            steps.push((f, Box::new(abi64_only)));
        }
        steps.push(("system/etc/init/hw/init.zygote64_32.rc", Box::new(zygote64_only)));
    }
    if !props.is_empty() { steps.push(("vendor/build.prop", Box::new(|t: &str| set_props(t, props)))); }
    if debug_logcat { steps.push(("system/etc/init/bootstat-debug.rc", Box::new(add_debug_logcat))); }

    let mut texts: Vec<(&str, String)> = vec![];
    for (rel, policy) in &steps {
        let cur = match texts.iter_mut().find(|(r, _)| r == rel) {
            Some((_, t)) => t,
            None => match fs::read_to_string(format!("{root}/{rel}")) {
                Ok(t) => { texts.push((rel, t)); &mut texts.last_mut().unwrap().1 }
                Err(_) => continue,
            },
        };
        *cur = policy(cur);
    }
    let mut binds = vec![];
    for (rel, text) in texts {
        let src = format!("{dir}/{}", rel.replace('/', "_"));
        fs::write(&src, text)?;
        binds.push((src, format!("{root}/{rel}")));
    }
    Ok(binds)
}

/// Dentro do mount ns do container: monta /dev, /proc, /sys sob `root` e faz pivot_root.
fn enter_rootfs(root: &str, binds: &[(String, String)], wayland_dir: Option<&str>) -> std::io::Result<()> {
    for (src, dst) in binds { mnt(src, dst, "", MS_BIND, "")?; }
    use std::os::unix::fs::symlink;
    let dev = format!("{root}/dev");
    mnt("tmpfs", &dev, "tmpfs", MS_NOSUID, "mode=0755")?;
    for n in DEV_NODES {
        let t = format!("{dev}/{n}");
        fs::write(&t, "")?;
        mnt(&format!("/dev/{n}"), &t, "", MS_BIND, "")?;
    }
    // stdio_to_kmsg do init Android escreve em /dev/kmsg_debug (criado pelo 1º estágio, que pulamos)
    fs::write(format!("{dev}/kmsg_debug"), "")?;
    mnt("/dev/kmsg", &format!("{dev}/kmsg_debug"), "", MS_BIND, "")?;
    fs::create_dir(format!("{dev}/pts"))?;
    mnt("devpts", &format!("{dev}/pts"), "devpts", MS_NOSUID | MS_NOEXEC, "newinstance,ptmxmode=0666,mode=0620")?;
    symlink("pts/ptmx", format!("{dev}/ptmx"))?;
    // binderfs próprio do container: binder/hwbinder/vndbinder isolados dos do host
    fs::create_dir(format!("{dev}/binderfs"))?;
    mnt("binder", &format!("{dev}/binderfs"), "binder", 0, "")?;
    // GPU: nós DRM próprios (mknod no tmpfs do container), 0666 como o ueventd.rc do Android faria.
    // Bind do host mudaria as permissões do nó do host junto.
    for n in ["card0", "renderD128"] {
        let Ok(majmin) = fs::read_to_string(format!("/sys/class/drm/{n}/dev")) else { continue };
        let Some((ma, mi)) = majmin.trim().split_once(':') else { continue };
        let (ma, mi): (u64, u64) = (ma.parse().unwrap_or(0), mi.parse().unwrap_or(0));
        fs::create_dir_all(format!("{dev}/dri"))?;
        let path = cstr(&format!("{dev}/dri/{n}"));
        let devno = (ma << 8) | (mi & 0xff) | ((mi & !0xff) << 12); // makedev glibc/musl
        if unsafe { mknod(path.as_ptr().cast(), 0o020666 /* S_IFCHR|0666 */, devno) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        fs::set_permissions(format!("{dev}/dri/{n}"), std::os::unix::fs::PermissionsExt::from_mode(0o666))?; // umask
    }
    // binderfs cria os nós 0600 root; serviços Android rodam como system/app e abortam com EACCES.
    // No Android o ueventd faria chmod 0666, mas binderfs não gera uevent.
    use std::os::unix::fs::PermissionsExt;
    for b in ["binder", "hwbinder", "vndbinder"] {
        fs::set_permissions(format!("{dev}/binderfs/{b}"), fs::Permissions::from_mode(0o666))?;
        symlink(format!("binderfs/{b}"), format!("{dev}/{b}"))?;
    }
    // O que o 1º estágio do init Android faria (pulamos: é trabalho do host)
    fs::create_dir(format!("{dev}/socket"))?;
    mnt("tmpfs", &format!("{root}/mnt"), "tmpfs", MS_NOSUID, "mode=0755")?;
    for d in ["vendor", "product"] { fs::create_dir(format!("{root}/mnt/{d}"))?; }
    // Compositor do host: hwcomposer do Waydroid procura $XDG_RUNTIME_DIR/wayland-0 = /run/user/1000
    if let Some(w) = wayland_dir {
        mnt("tmpfs", &format!("{root}/run"), "tmpfs", MS_NOSUID, "mode=0755")?;
        let user = format!("{root}/run/user/1000");
        fs::create_dir_all(&user)?;
        mnt(w, &user, "", MS_BIND, "")?;
        // read-only: o container conecta no socket (sockets ignoram ro) mas não pode trocá-lo
        mnt("none", &user, "", MS_REMOUNT | MS_BIND | MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC, "")?;
    }
    mnt("proc", &format!("{root}/proc"), "proc", MS_NOSUID | MS_NOEXEC, "")?;
    mnt("sysfs", &format!("{root}/sys"), "sysfs", MS_NOSUID | MS_NOEXEC, "")?;
    // getcon()/selinux_status_open() do servicemanager precisam disso; kernel sem SELinux = ignora
    let _ = mnt("selinuxfs", &format!("{root}/sys/fs/selinux"), "selinuxfs", MS_NOSUID | MS_NOEXEC, "");
    // pivot_root(".", "."): empilha raiz antiga sob a nova; umount lazy a remove.
    mnt(root, root, "", MS_BIND | MS_REC, "")?;
    let r = cstr(root);
    let dot = cstr(".");
    unsafe {
        if chdir(r.as_ptr().cast()) != 0 || syscall(SYS_PIVOT_ROOT, dot.as_ptr(), dot.as_ptr()) != 0
            || umount2(dot.as_ptr().cast(), MNT_DETACH) != 0 || chdir(c"/".as_ptr().cast()) != 0 {
            let e = std::io::Error::last_os_error();
            return Err(std::io::Error::new(e.kind(), format!("pivot_root {root}: {e}")));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State { Stopped, Running, Frozen }

struct Container { payload: Vec<String>, rootfs: Option<String>, wayland_dir: Option<String>, abi64: bool, debug_logcat: bool, props: Vec<(String, String)>, child: Option<Child>, state: State, idle_freeze: Duration, close_at: Option<Instant> }

fn cg(file: &str) -> String { format!("{CG}/{file}") }

/// Linha de `cgroup.events` / `memory.stat`-like: "chave valor".
fn kv(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')?.trim().parse().ok())
}

impl Container {
    fn mem_kb(&self) -> u64 { fs::read_to_string(cg("memory.current")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0) / 1024 }

    fn status(&self) -> String {
        let pid = self.child.as_ref().map_or(0, |c| c.id());
        format!("state={:?} mem_kb={} pid={pid}", self.state, self.mem_kb()).to_lowercase()
    }

    fn start(&mut self) -> Result<(), String> {
        fs::create_dir_all(CG).map_err(|e| format!("cgroup: {e}"))?;
        let procs = cg("cgroup.procs");
        let rootfs = self.rootfs.clone();
        let wayland_dir = self.wayland_dir.clone();
        let binds = match &rootfs {
            Some(r) => prop_overrides(r, self.abi64, self.debug_logcat, &self.props).map_err(|e| format!("overrides: {e}"))?,
            None => vec![],
        };
        let mut cmd = Command::new(&self.payload[0]);
        cmd.args(&self.payload[1..]);
        use std::os::unix::process::CommandExt;
        // Pós-fork, pré-exec (daemon é single-thread). unshare(NEWPID) só vale p/ filhos, então
        // bifurca: o neto vira PID 1 do container; o intermediário espera e repassa o status.
        unsafe {
            cmd.pre_exec(move || {
                fs::write(&procs, "0")?;
                if unshare(CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWNET | CLONE_NEWPID) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let pid = fork();
                if pid < 0 { return Err(std::io::Error::last_os_error()); }
                if pid > 0 {
                    // intermediário nunca faz exec: fecha o pipe CLOEXEC do std (senão spawn() espera p/ sempre)
                    // ponytail: varre até 1024; close_range se o limite de fds subir
                    for fd in 3..1024 { close(fd); }
                    let mut st = 0;
                    waitpid(pid, &mut st, 0);
                    _exit(if st & 0x7f == 0 { (st >> 8) & 0xff } else { 128 + (st & 0x7f) });
                }
                // mounts do container não vazam p/ o host
                mnt("none", "/", "", MS_REC | MS_PRIVATE, "")?;
                match &rootfs {
                    Some(r) => enter_rootfs(r, &binds, wayland_dir.as_deref())?,
                    // modo teste: mesma raiz do host (read-only), só /proc próprio
                    None => mnt("proc", "/proc", "proc", 0, "")?,
                }
                sethostname(b"langedroid".as_ptr(), 10);
                Ok(())
            })
        };
        self.child = Some(cmd.spawn().map_err(|e| format!("spawn: {e}"))?);
        self.state = State::Running;
        Ok(())
    }

    fn set_frozen(&mut self, on: bool) -> Result<(), String> {
        if self.state == State::Stopped { return Err("container parado".into()); }
        fs::write(cg("cgroup.freeze"), if on { "1" } else { "0" }).map_err(|e| format!("cgroup.freeze: {e}"))?;
        // congelar é assíncrono: espera cgroup.events refletir (até 1s)
        let want = on as u64;
        let t = Instant::now();
        while kv(&fs::read_to_string(cg("cgroup.events")).unwrap_or_default(), "frozen") != Some(want) {
            if t.elapsed() > Duration::from_secs(1) { return Err("timeout aguardando freeze".into()); }
            thread::sleep(Duration::from_millis(10));
        }
        self.state = if on { State::Frozen } else { State::Running };
        Ok(())
    }

    fn stop(&mut self) {
        let _ = fs::write(cg("cgroup.kill"), "1"); // mata container inteiro, inclusive o PID 1 dele
        if let Some(mut c) = self.child.take() { let _ = c.wait(); }
        let _ = fs::write(cg("cgroup.freeze"), "0");
        self.state = State::Stopped;
        self.close_at = None;
    }

    fn handle(&mut self, cmd: &str) -> String {
        let r = match cmd {
            "open" => {
                self.close_at = None;
                match self.state { State::Stopped => self.start(), State::Frozen => self.set_frozen(false), State::Running => Ok(()) }
            }
            "close" => { if self.state == State::Running { self.close_at = Some(Instant::now() + self.idle_freeze) } Ok(()) }
            "freeze" => self.set_frozen(true),
            "thaw" => self.set_frozen(false),
            "stop" => { self.stop(); Ok(()) }
            "status" => Ok(()),
            o => Err(format!("comando desconhecido: {o}")),
        };
        match r { Ok(()) => format!("ok {}", self.status()), Err(e) => format!("erro {e}") }
    }

    /// Timer de ociosidade + detecção de container que morreu sozinho.
    fn tick(&mut self) {
        if let Some(c) = &mut self.child && matches!(c.try_wait(), Ok(Some(_))) {
            eprintln!("[langedroid] container saiu");
            self.stop();
        }
        if self.close_at.is_some_and(|t| Instant::now() >= t) {
            self.close_at = None;
            match self.set_frozen(true) {
                Ok(()) => {
                    // freeze só para a CPU; RAM continua ocupada. memory.reclaim empurra as páginas p/ zram.
                    let before = self.mem_kb();
                    let _ = fs::write(cg("memory.reclaim"), (before * 1024).to_string()); // EAGAIN = reclaim parcial, ok
                    eprintln!("[langedroid] ocioso: congelado, RAM {before} KB -> {} KB", self.mem_kb());
                }
                Err(e) => eprintln!("[langedroid] auto-freeze: {e}"),
            }
        }
    }
}

fn daemon(args: &[String]) {
    let mut idle = 30u64;
    let mut payload = vec![];
    let mut rootfs = None;
    let mut abi64 = true;
    let mut debug_logcat = false;
    let mut props = vec![];
    let mut wayland_dir = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--idle-freeze" => idle = it.next().and_then(|v| v.parse().ok()).expect("--idle-freeze SEG"),
            "--allow-32bit" => abi64 = false,
            "--debug-logcat" => debug_logcat = true,
            "--wayland-dir" => wayland_dir = Some(it.next().expect("--wayland-dir DIR").clone()),
            "--prop" => {
                let kv = it.next().and_then(|v| v.split_once('=')).expect("--prop chave=valor");
                props.push((kv.0.to_string(), kv.1.to_string()));
            }
            "--rootfs" => rootfs = Some(it.next().expect("--rootfs DIR").clone()),
            "--payload" => { payload = it.by_ref().cloned().collect(); }
            o => { eprintln!("[langedroid] argumento desconhecido: {o}"); std::process::exit(2); }
        }
    }
    if payload.is_empty() { payload = vec![if rootfs.is_some() { "/init" } else { "/bin/droid-payload" }.to_string()]; }
    let _ = fs::remove_file(SOCK);
    // Socket de controle só p/ root: quem fala com ele controla o container.
    // umask antes do bind: o socket já nasce 0600 (sem janela entre bind e chmod).
    let old = unsafe { umask(0o177) };
    let l = UnixListener::bind(SOCK).unwrap_or_else(|e| { eprintln!("[langedroid] bind {SOCK}: {e}"); std::process::exit(1) });
    unsafe { umask(old) };
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = fs::set_permissions(SOCK, fs::Permissions::from_mode(0o600)) {
        eprintln!("[langedroid] chmod {SOCK}: {e}"); std::process::exit(1);
    }
    l.set_nonblocking(true).unwrap();
    let mut c = Container { payload, rootfs, wayland_dir, abi64, debug_logcat, props, child: None, state: State::Stopped, idle_freeze: Duration::from_secs(idle), close_at: None };
    eprintln!("[langedroid] pronto em {SOCK} (cold start sob demanda, freeze após {idle}s ocioso)");
    loop {
        match l.accept() {
            Ok((s, _)) => {
                // defesa em profundidade além do 0600: só aceita peer root
                if peer_uid(&s) != Some(0) { eprintln!("[langedroid] conexão recusada: uid {:?}", peer_uid(&s)); continue; }
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(&s).read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    let reply = c.handle(line.trim());
                    eprintln!("[langedroid] {} -> {reply}", line.trim());
                    let _ = writeln!(&s, "{reply}");
                }
            }
            // ponytail: polling 100ms; trocar por epoll+timerfd quando medir bateria
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
        c.tick();
    }
}

/// uid do processo do outro lado (SO_PEERCRED; struct ucred = pid, uid, gid).
fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = [0u32; 3];
    let mut len = 12u32;
    let r = unsafe { getsockopt(s.as_raw_fd(), 1 /* SOL_SOCKET */, 17 /* SO_PEERCRED */, cred.as_mut_ptr().cast(), &mut len) };
    (r == 0 && len == 12).then_some(cred[1])
}

/// Espera o daemon abrir o socket (serviços sobem em paralelo no boot).
fn wait_sock(max: Duration) -> bool {
    let t = Instant::now();
    while UnixStream::connect(SOCK).is_err() {
        if t.elapsed() > max { return false; }
        thread::sleep(Duration::from_millis(100));
    }
    true
}

fn ctl(cmd: &str) -> std::io::Result<String> {
    wait_sock(Duration::from_secs(5));
    let mut s = UnixStream::connect(SOCK)?;
    writeln!(s, "{cmd}")?;
    let mut r = String::new();
    BufReader::new(s).read_line(&mut r)?;
    Ok(r.trim().into())
}

fn field<'a>(reply: &'a str, key: &str) -> &'a str {
    reply.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix('=')).unwrap_or("")
}

fn selftest() -> bool {
    if !wait_sock(Duration::from_secs(5)) { eprintln!("[selftest] FAIL daemon não respondeu"); return false; }
    let ok = std::cell::Cell::new(true);
    let step = |name: &str, cmd: &str, want_state: &str, wait: u64| {
        thread::sleep(Duration::from_secs(wait));
        let r = ctl(cmd).unwrap_or_else(|e| format!("erro {e}"));
        let pass = r.starts_with("ok") && field(&r, "state") == want_state;
        ok.set(ok.get() && pass);
        eprintln!("[selftest] {} {name}: {r}", if pass { "PASS" } else { "FAIL" });
        r
    };
    step("inicia parado (sem cold start no boot)", "status", "stopped", 0);
    step("open = cold start", "open", "running", 0);
    let mem_running: u64 = field(&step("container consome RAM", "status", "running", 2), "mem_kb").parse().unwrap_or(0);
    if mem_running < 8 * 1024 { ok.set(false); eprintln!("[selftest] FAIL esperado >= 8 MB no cgroup, veio {mem_running} KB"); }
    step("freeze manual", "freeze", "frozen", 0);
    step("thaw manual", "thaw", "running", 2);
    step("close arma timer", "close", "running", 0);
    let mem_idle: u64 = field(&step("auto-freeze após ocioso", "status", "frozen", 3), "mem_kb").parse().unwrap_or(u64::MAX);
    if mem_idle * 2 > mem_running { ok.set(false); eprintln!("[selftest] FAIL ocioso deveria liberar RAM: {mem_running} KB -> {mem_idle} KB"); }
    else { eprintln!("[selftest] PASS ocioso liberou RAM: {mem_running} KB -> {mem_idle} KB"); }
    step("open descongela", "open", "running", 0);
    step("stop", "stop", "stopped", 1);
    eprintln!("[selftest] {}", if ok.get() { "TUDO PASSOU" } else { "FALHOU" });
    ok.get()
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => daemon(&args[1..]),
        Some("ctl") if args.len() == 2 => match ctl(&args[1]) {
            Ok(r) => println!("{r}"),
            Err(e) => { eprintln!("langedroid: {SOCK}: {e}"); std::process::exit(1) }
        },
        Some("selftest") => std::process::exit(if selftest() { 0 } else { 1 }),
        _ => { eprintln!("uso: langedroid daemon [--idle-freeze SEG] [--rootfs DIR] [--allow-32bit] [--debug-logcat] [--prop K=V]... [--wayland-dir DIR] [--payload CMD...] | ctl CMD | selftest"); std::process::exit(2) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsing() {
        assert_eq!(kv("populated 1\nfrozen 1\n", "frozen"), Some(1));
        assert_eq!(kv("frozenx 1\nfrozen 0", "frozen"), Some(0));
        assert_eq!(field("ok state=frozen mem_kb=9000 pid=3", "mem_kb"), "9000");
        assert_eq!(field("ok state=frozen", "pid"), "");
    }
    #[test]
    fn abi64() {
        let p = abi64_only("# c\nro.vendor.product.cpu.abilist=arm64-v8a,armeabi-v7a,armeabi\nro.vendor.product.cpu.abilist32=armeabi-v7a,armeabi\nro.vendor.product.cpu.abilist64=arm64-v8a\nro.zygote=zygote64_32\nfoo=bar\n");
        assert_eq!(p, "# c\nro.vendor.product.cpu.abilist=arm64-v8a\nro.vendor.product.cpu.abilist64=arm64-v8a\nro.zygote=zygote64_32\nfoo=bar\n");
        let props = [("ro.hardware.egl".to_string(), "mesa".to_string()), ("new.k".to_string(), "1".to_string())];
        assert_eq!(set_props("a=1\nro.hardware.egl=x\n", &props), "a=1\nro.hardware.egl=mesa\nnew.k=1\n");
        let rc = "service zygote /a64\n    class main\n\nservice zygote_secondary /a32\n    class main\n";
        assert_eq!(zygote64_only(rc), "service zygote /a64\n    class main\n\n");
    }
}
