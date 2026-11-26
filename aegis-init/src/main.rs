//! aegis-init: PID 1 + service supervisor for AegisOS (Blueprint Fase 1).
//! Zero dependencies. Service files: `<dir>/*.svc`, lines `key=value`:
//!   exec=/usr/bin/foo --flag   (required)
//!   restart=always|never       (default always)
//!   min_profile=ultralight|mid|flagship (default ultralight = runs everywhere)
//!   memory_max=64M             (opcional; cgroup v2 memory.max e memory.swap.max, só como PID 1)
//! Como PID 1 também: monta /etc/aegis/fstab, liga swap ZRAM, grava log em /data/log/init.log.
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

/// Log: console + /data/log/init.log (quando /data montar; linhas anteriores ficam em buffer).
static LOG: Mutex<(Vec<String>, Option<fs::File>)> = Mutex::new((Vec::new(), None));
macro_rules! log {
    ($($t:tt)*) => {{
        let line = format!("[init] {}", format!($($t)*));
        eprintln!("{line}");
        let mut g = LOG.lock().unwrap();
        match &mut g.1 { Some(f) => { let _ = writeln!(f, "{line}"); } None => g.0.push(line) }
    }};
}

fn open_log_file(path: &str) {
    if let Some(dir) = std::path::Path::new(path).parent() { let _ = fs::create_dir_all(dir); }
    let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let mut g = LOG.lock().unwrap();
    for l in g.0.drain(..) { let _ = writeln!(f, "{l}"); }
    g.1 = Some(f);
}

#[derive(Debug, PartialEq)]
struct MountEntry { src: String, target: String, fstype: String, ro: bool }

/// fstab mínimo: `origem destino tipo [ro|rw]` por linha.
fn parse_fstab(text: &str) -> Result<Vec<MountEntry>, String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        let ro = match f.get(3).copied() { None | Some("rw") => false, Some("ro") => true, Some(o) => return Err(format!("fstab: opção {o}")) };
        match f[..] {
            [src, target, fstype, ..] if f.len() <= 4 => Ok(MountEntry { src: src.into(), target: target.into(), fstype: fstype.into(), ro }),
            _ => Err(format!("fstab: linha inválida: {l}")),
        }
    }).collect()
}

/// Cabeçalho de swap Linux (formato v1, "SWAPSPACE2") para um dispositivo de `size` bytes.
fn swap_header(size: u64, page: usize) -> Vec<u8> {
    let mut h = vec![0u8; page];
    let last_page = (size / page as u64 - 1) as u32;
    h[1024..1028].copy_from_slice(&1u32.to_ne_bytes()); // version
    h[1028..1032].copy_from_slice(&last_page.to_ne_bytes());
    h[page - 10..].copy_from_slice(b"SWAPSPACE2");
    h
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
enum Profile { UltraLight, Mid, Flagship }

impl Profile {
    fn from_mem_kb(kb: u64) -> Self {
        match kb / 1024 {
            0..=2304 => Profile::UltraLight, // <= ~2 GB (tolerância p/ RAM reservada)
            2305..=4608 => Profile::Mid,
            _ => Profile::Flagship,
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s { "ultralight" => Some(Self::UltraLight), "mid" => Some(Self::Mid), "flagship" => Some(Self::Flagship), _ => None }
    }
}

/// Aceita "max" ou número com sufixo opcional K/M/G (formato do memory.max do kernel).
fn parse_mem(v: &str) -> Option<String> {
    let digits = v.strip_suffix(['K', 'M', 'G']).unwrap_or(v);
    (v == "max" || (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))).then(|| v.into())
}

fn mem_total_kb(meminfo: &str) -> Option<u64> {
    meminfo.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()
}

#[derive(Debug, PartialEq)]
struct Service { name: String, argv: Vec<String>, restart: bool, min_profile: Profile, memory_max: Option<String> }

fn parse_service(name: &str, text: &str) -> Result<Service, String> {
    let (mut argv, mut restart, mut min_profile, mut memory_max) = (Vec::new(), true, Profile::UltraLight, None);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (k, v) = line.split_once('=').ok_or(format!("{name}: linha inválida: {line}"))?;
        match k.trim() {
            "exec" => argv = v.split_whitespace().map(String::from).collect(),
            "restart" => restart = match v.trim() { "always" => true, "never" => false, o => return Err(format!("{name}: restart={o}")) },
            "min_profile" => min_profile = Profile::parse(v.trim()).ok_or(format!("{name}: min_profile={v}"))?,
            "memory_max" => memory_max = Some(parse_mem(v.trim()).ok_or(format!("{name}: memory_max={v}"))?),
            o => return Err(format!("{name}: chave desconhecida {o}")),
        }
    }
    if argv.is_empty() { return Err(format!("{name}: exec ausente")); }
    Ok(Service { name: name.into(), argv, restart, min_profile, memory_max })
}

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn mount(src: *const u8, target: *const u8, fstype: *const u8, flags: usize, data: *const u8) -> i32;
    fn dup2(old: i32, new: i32) -> i32;
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    fn siginterrupt(sig: i32, flag: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn pause() -> i32;
    fn sync();
    fn reboot(cmd: i32) -> i32;
    fn swapon(path: *const u8, flags: i32) -> i32;
    fn getpagesize() -> i32;
}

const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGUSR1: i32 = 10;
const WNOHANG: i32 = 1;
const RB_POWER_OFF: i32 = 0x4321fedcu32 as i32;
const RB_AUTOBOOT: i32 = 0x01234567;

/// Sinal de desligamento pendente (0 = nenhum). Convenção sysvinit/busybox:
/// SIGTERM/SIGUSR1 = poweroff, SIGINT (Ctrl-Alt-Del) = reboot.
static SHUTDOWN: AtomicI32 = AtomicI32::new(0);
extern "C" fn on_signal(sig: i32) { SHUTDOWN.store(sig, Ordering::SeqCst); }

fn install_signals() {
    for sig in [SIGTERM, SIGUSR1, SIGINT] {
        // siginterrupt: waitpid/pause retornam EINTR em vez de reiniciar (musl/glibc usam SA_RESTART)
        unsafe { signal(sig, on_signal); siginterrupt(sig, 1); }
    }
}

fn shutdown(pid1: bool, running: &HashMap<i32, (usize, Instant)>, sig: i32) -> ! {
    let reboot_after = sig == SIGINT;
    log!("desligando ({})", if reboot_after { "reboot" } else { "poweroff" });
    // PID 1 mata tudo; fora dele (dev no host) só os filhos supervisionados — nunca kill(-1)!
    let targets: Vec<i32> = if pid1 { vec![-1] } else { running.keys().copied().collect() };
    for &t in &targets { unsafe { kill(t, SIGTERM) }; }
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut st = 0;
        let r = unsafe { waitpid(-1, &mut st, WNOHANG) };
        if r < 0 { break; } // ECHILD: todos saíram
        if r == 0 { thread::sleep(Duration::from_millis(50)); }
    }
    for &t in &targets { unsafe { kill(t, SIGKILL) }; }
    { let mut g = LOG.lock().unwrap(); if let Some(f) = &mut g.1 { let _ = f.sync_all(); } }
    unsafe { sync() };
    if !pid1 { std::process::exit(0); }
    unsafe { reboot(if reboot_after { RB_AUTOBOOT } else { RB_POWER_OFF }) };
    loop { unsafe { pause() }; } // reboot falhou: PID 1 não pode sair (kernel panic)
}

fn c(s: &str) -> Vec<u8> { let mut v = s.as_bytes().to_vec(); v.push(0); v }

fn mount_fstab(path: &str) {
    let Ok(text) = fs::read_to_string(path) else { return };
    match parse_fstab(&text) {
        Err(e) => log!("{e}"),
        Ok(entries) => for m in entries {
            let _ = fs::create_dir_all(&m.target);
            let r = unsafe { mount(c(&m.src).as_ptr(), c(&m.target).as_ptr(), c(&m.fstype).as_ptr(), m.ro as usize /* MS_RDONLY */, std::ptr::null()) };
            if r == 0 { log!("montado {} em {} ({})", m.src, m.target, m.fstype) }
            else { log!("falha montando {} em {}: {}", m.src, m.target, std::io::Error::last_os_error()) }
        },
    }
}

/// Swap comprimido em RAM. ponytail: tamanho = metade da RAM p/ todo perfil; ajustar por perfil após medir.
fn setup_zram(mem_kb: u64) {
    let z = "/sys/block/zram0";
    if !std::path::Path::new(z).exists() { return log!("zram indisponível"); }
    let size = mem_kb * 1024 / 2;
    let _ = fs::write(format!("{z}/comp_algorithm"), "lz4");
    if let Err(e) = fs::write(format!("{z}/disksize"), size.to_string()) { return log!("zram disksize: {e}"); }
    let page = unsafe { getpagesize() } as usize;
    if let Err(e) = fs::OpenOptions::new().write(true).open("/dev/zram0").and_then(|mut f| f.write_all(&swap_header(size, page))) {
        return log!("zram header: {e}");
    }
    if unsafe { swapon(c("/dev/zram0").as_ptr(), 0) } != 0 { return log!("swapon: {}", std::io::Error::last_os_error()); }
    log!("zram swap ativo: {} MB lz4", size >> 20);
}

fn mount_early() {
    for (src, tgt, fs) in [("proc\0", "/proc\0", "proc\0"), ("sysfs\0", "/sys\0", "sysfs\0"), ("devtmpfs\0", "/dev\0", "devtmpfs\0"),
                             ("cgroup2\0", "/sys/fs/cgroup\0", "cgroup2\0")] {
        let _ = fs::create_dir_all(tgt.trim_end_matches('\0'));
        // ponytail: erro ignorado (já montado pelo kernel/initramfs é o caso comum)
        unsafe { mount(src.as_ptr(), tgt.as_ptr(), fs.as_ptr(), 0, std::ptr::null()) };
    }
    // initramfs sem /dev/console: kernel não abriu stdio p/ nós. Reabre após devtmpfs.
    if let Ok(c) = fs::OpenOptions::new().read(true).write(true).open("/dev/console") {
        use std::os::fd::AsRawFd;
        for fd in 0..3 { unsafe { dup2(c.as_raw_fd(), fd) }; }
    }
}

const CGROOT: &str = "/sys/fs/cgroup";

/// Cria /sys/fs/cgroup/<serviço> com limites. None se cgroup indisponível (fora do PID 1).
fn setup_cgroup(s: &Service) -> Option<String> {
    let dir = format!("{CGROOT}/{}", s.name);
    fs::create_dir_all(&dir).ok()?;
    if let Some(m) = &s.memory_max {
        // swap.max = mesmo valor: sem isso, com zram, o excesso vai p/ swap e o limite vira ilimitado.
        // Serviço pode usar até memory_max em RAM + memory_max em zram.
        for f in ["memory.max", "memory.swap.max"] {
            if let Err(e) = fs::write(format!("{dir}/{f}"), m) { log!("{}: {f}: {e}", s.name); }
        }
    }
    Some(format!("{dir}/cgroup.procs"))
}

fn spawn(s: &Service, cgroups: bool) -> Option<i32> {
    let mut cmd = Command::new(&s.argv[0]);
    cmd.args(&s.argv[1..]);
    if let Some(procs) = cgroups.then(|| setup_cgroup(s)).flatten() {
        use std::os::unix::process::CommandExt;
        // filho entra no cgroup antes do exec: netos não escapam. init é single-thread, então
        // alocar aqui (pós-fork) é seguro.
        unsafe { cmd.pre_exec(move || fs::write(&procs, "0")) };
    }
    match cmd.spawn() {
        Ok(c) => { log!("start {} pid={}", s.name, c.id()); Some(c.id() as i32) }
        Err(e) => { log!("falha {}: {e}", s.name); None }
    }
}

fn main() {
    let pid1 = std::process::id() == 1;
    if pid1 { mount_early(); }
    // ponytail: só memory; cpu/io quando houver política de prioridade (perfis)
    let cgroups = pid1 && fs::write(format!("{CGROOT}/cgroup.subtree_control"), "+memory").is_ok();
    if pid1 && !cgroups { log!("cgroup v2 indisponível; serviços sem limites"); }
    install_signals();
    let dir = env::args().nth(1).unwrap_or("/etc/aegis/services".into());
    let mem_kb = fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(mem_total_kb);
    let profile = mem_kb.map(Profile::from_mem_kb).unwrap_or(Profile::UltraLight);
    if pid1 {
        mount_fstab("/etc/aegis/fstab");
        open_log_file("/data/log/init.log");
        if let Some(kb) = mem_kb { setup_zram(kb); }
    }
    log!("perfil de hardware: {profile:?}");

    let mut services: Vec<Service> = fs::read_dir(&dir).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "svc"))
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            parse_service(&name, &fs::read_to_string(&p).ok()?).map_err(|e| log!("{e}")).ok()
        })
        .filter(|s| profile >= s.min_profile)
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name)); // ordem determinística: prefixe 00-, 10-...

    let mut running: HashMap<i32, (usize, Instant)> = HashMap::new();
    for (i, s) in services.iter().enumerate() {
        if let Some(pid) = spawn(s, cgroups) { running.insert(pid, (i, Instant::now())); }
    }

    loop {
        let sig = SHUTDOWN.load(Ordering::SeqCst);
        if sig != 0 { shutdown(pid1, &running, sig); }
        let mut status = 0;
        let pid = unsafe { waitpid(-1, &mut status, 0) };
        if pid < 0 { // EINTR (sinal) ou ECHILD (nada para supervisionar)
            if SHUTDOWN.load(Ordering::SeqCst) != 0 { continue; }
            if pid1 { unsafe { pause() }; continue; } // PID 1 nunca sai; acorda em sinal
            return;
        }
        let Some((i, started)) = running.remove(&pid) else { continue }; // órfão reaped
        let s = &services[i];
        log!("{} saiu (status={status})", s.name);
        if !s.restart { continue; }
        // ponytail: backoff fixo 1s p/ crash-loop; exponencial se necessário
        if started.elapsed() < Duration::from_secs(1) { thread::sleep(Duration::from_secs(1)); }
        if let Some(p) = spawn(s, cgroups) { running.insert(p, (i, Instant::now())); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles() {
        assert_eq!(Profile::from_mem_kb(1_000_000), Profile::UltraLight);
        assert_eq!(Profile::from_mem_kb(3_900_000), Profile::Mid);
        assert_eq!(Profile::from_mem_kb(8_000_000), Profile::Flagship);
        assert_eq!(mem_total_kb("MemTotal:  1953124 kB\nMemFree: 1 kB"), Some(1953124));
    }
    #[test]
    fn service_file() {
        let s = parse_service("droid", "# c\nexec=/bin/aegisdroid --lazy\nrestart=never\nmin_profile=mid").unwrap();
        assert_eq!(s.argv, ["/bin/aegisdroid", "--lazy"]);
        assert!(!s.restart && s.min_profile == Profile::Mid);
        assert!(parse_service("x", "restart=always").is_err());
        assert!(parse_service("x", "exec=/a\nfoo=1").is_err());
        assert_eq!(parse_service("x", "exec=/a\nmemory_max=64M").unwrap().memory_max.as_deref(), Some("64M"));
        assert!(parse_service("x", "exec=/a\nmemory_max=64MB").is_err());
        assert!(parse_service("x", "exec=/a\nmemory_max=../x").is_err());
    }
    #[test]
    fn fstab() {
        let m = parse_fstab("# c\n/dev/vdb /data ext4\n/dev/vdc /mnt erofs ro\n").unwrap();
        assert_eq!(m[0], MountEntry { src: "/dev/vdb".into(), target: "/data".into(), fstype: "ext4".into(), ro: false });
        assert!(m[1].ro);
        assert!(parse_fstab("/dev/vdb /data").is_err());
        assert!(parse_fstab("/a /b ext4 noexec").is_err());
        assert!(parse_fstab("/a /b ext4 ro extra").is_err());
    }
    #[test]
    fn swap() {
        let h = swap_header(64 << 20, 4096);
        assert_eq!(&h[4086..], b"SWAPSPACE2");
        assert_eq!(u32::from_ne_bytes(h[1028..1032].try_into().unwrap()), 16383);
    }
}
