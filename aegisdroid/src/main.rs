//! aegisdroid: orquestrador do container Android do AegisOS (Blueprint §4A, §5A). Zero dependências.
//!
//!   aegisdroid daemon [--idle-freeze SEG] [--payload CMD...]   serviço (roda sob aegis-init, como root)
//!   aegisdroid ctl <open|close|freeze|thaw|status|stop>         cliente
//!   aegisdroid selftest                                          teste ponta a ponta contra o daemon
//!
//! Container = payload em namespaces novos (pid, mount, uts, ipc, net) dentro de /sys/fs/cgroup/android.
//! Ciclo (perfil Ultra-Light): `open` faz cold start ou descongela; `close` congela após SEG ocioso.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

const SOCK: &str = "/run/aegisdroid.sock";
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
}
const CLONE_NEWNS: i32 = 0x0002_0000;
const CLONE_NEWUTS: i32 = 0x0400_0000;
const CLONE_NEWIPC: i32 = 0x0800_0000;
const CLONE_NEWPID: i32 = 0x2000_0000;
const CLONE_NEWNET: i32 = 0x4000_0000;
const MS_REC: usize = 0x4000;
const MS_PRIVATE: usize = 0x4_0000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum State { Stopped, Running, Frozen }

struct Container { payload: Vec<String>, child: Option<Child>, state: State, idle_freeze: Duration, close_at: Option<Instant> }

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
                // ponytail: mesma raiz do host (só leitura); pivot_root p/ imagem Android na etapa 6.
                mount(c"none".as_ptr().cast(), c"/".as_ptr().cast(), std::ptr::null(), MS_REC | MS_PRIVATE, std::ptr::null());
                mount(c"proc".as_ptr().cast(), c"/proc".as_ptr().cast(), c"proc".as_ptr().cast(), 0, std::ptr::null());
                sethostname(b"aegisdroid".as_ptr(), 10);
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
            eprintln!("[aegisdroid] container saiu");
            self.stop();
        }
        if self.close_at.is_some_and(|t| Instant::now() >= t) {
            self.close_at = None;
            match self.set_frozen(true) {
                Ok(()) => {
                    // freeze só para a CPU; RAM continua ocupada. memory.reclaim empurra as páginas p/ zram.
                    let before = self.mem_kb();
                    let _ = fs::write(cg("memory.reclaim"), (before * 1024).to_string()); // EAGAIN = reclaim parcial, ok
                    eprintln!("[aegisdroid] ocioso: congelado, RAM {before} KB -> {} KB", self.mem_kb());
                }
                Err(e) => eprintln!("[aegisdroid] auto-freeze: {e}"),
            }
        }
    }
}

fn daemon(args: &[String]) {
    let mut idle = 30u64;
    let mut payload = vec!["/bin/droid-payload".to_string()];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--idle-freeze" => idle = it.next().and_then(|v| v.parse().ok()).expect("--idle-freeze SEG"),
            "--payload" => { payload = it.by_ref().cloned().collect(); }
            o => { eprintln!("[aegisdroid] argumento desconhecido: {o}"); std::process::exit(2); }
        }
    }
    let _ = fs::remove_file(SOCK);
    // Socket de controle só p/ root: quem fala com ele controla o container.
    // umask antes do bind: o socket já nasce 0600 (sem janela entre bind e chmod).
    let old = unsafe { umask(0o177) };
    let l = UnixListener::bind(SOCK).unwrap_or_else(|e| { eprintln!("[aegisdroid] bind {SOCK}: {e}"); std::process::exit(1) });
    unsafe { umask(old) };
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = fs::set_permissions(SOCK, fs::Permissions::from_mode(0o600)) {
        eprintln!("[aegisdroid] chmod {SOCK}: {e}"); std::process::exit(1);
    }
    l.set_nonblocking(true).unwrap();
    let mut c = Container { payload, child: None, state: State::Stopped, idle_freeze: Duration::from_secs(idle), close_at: None };
    eprintln!("[aegisdroid] pronto em {SOCK} (cold start sob demanda, freeze após {idle}s ocioso)");
    loop {
        match l.accept() {
            Ok((s, _)) => {
                // defesa em profundidade além do 0600: só aceita peer root
                if peer_uid(&s) != Some(0) { eprintln!("[aegisdroid] conexão recusada: uid {:?}", peer_uid(&s)); continue; }
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(&s).read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    let reply = c.handle(line.trim());
                    eprintln!("[aegisdroid] {} -> {reply}", line.trim());
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

fn ctl(cmd: &str) -> std::io::Result<String> {
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
    let t = Instant::now();
    while UnixStream::connect(SOCK).is_err() {
        if t.elapsed() > Duration::from_secs(5) { eprintln!("[selftest] FAIL daemon não respondeu"); return false; }
        thread::sleep(Duration::from_millis(100));
    }
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
            Err(e) => { eprintln!("aegisdroid: {SOCK}: {e}"); std::process::exit(1) }
        },
        Some("selftest") => std::process::exit(if selftest() { 0 } else { 1 }),
        _ => { eprintln!("uso: aegisdroid daemon [--idle-freeze SEG] [--payload CMD...] | ctl CMD | selftest"); std::process::exit(2) }
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
}
