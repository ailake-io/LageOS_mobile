//! aegis-init: PID 1 + service supervisor for AegisOS (Blueprint Fase 1).
//! Zero dependencies. Service files: `<dir>/*.svc`, lines `key=value`:
//!   exec=/usr/bin/foo --flag   (required)
//!   restart=always|never       (default always)
//!   min_profile=ultralight|mid|flagship (default ultralight = runs everywhere)
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

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

fn mem_total_kb(meminfo: &str) -> Option<u64> {
    meminfo.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse().ok()
}

#[derive(Debug, PartialEq)]
struct Service { name: String, argv: Vec<String>, restart: bool, min_profile: Profile }

fn parse_service(name: &str, text: &str) -> Result<Service, String> {
    let (mut argv, mut restart, mut min_profile) = (Vec::new(), true, Profile::UltraLight);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (k, v) = line.split_once('=').ok_or(format!("{name}: linha inválida: {line}"))?;
        match k.trim() {
            "exec" => argv = v.split_whitespace().map(String::from).collect(),
            "restart" => restart = match v.trim() { "always" => true, "never" => false, o => return Err(format!("{name}: restart={o}")) },
            "min_profile" => min_profile = Profile::parse(v.trim()).ok_or(format!("{name}: min_profile={v}"))?,
            o => return Err(format!("{name}: chave desconhecida {o}")),
        }
    }
    if argv.is_empty() { return Err(format!("{name}: exec ausente")); }
    Ok(Service { name: name.into(), argv, restart, min_profile })
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
    eprintln!("[init] desligando ({})", if reboot_after { "reboot" } else { "poweroff" });
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
    unsafe { sync() };
    if !pid1 { std::process::exit(0); }
    unsafe { reboot(if reboot_after { RB_AUTOBOOT } else { RB_POWER_OFF }) };
    loop { unsafe { pause() }; } // reboot falhou: PID 1 não pode sair (kernel panic)
}

fn mount_early() {
    for (src, tgt, fs) in [("proc\0", "/proc\0", "proc\0"), ("sysfs\0", "/sys\0", "sysfs\0"), ("devtmpfs\0", "/dev\0", "devtmpfs\0")] {
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

fn spawn(s: &Service) -> Option<i32> {
    match Command::new(&s.argv[0]).args(&s.argv[1..]).spawn() {
        Ok(c) => { eprintln!("[init] start {} pid={}", s.name, c.id()); Some(c.id() as i32) }
        Err(e) => { eprintln!("[init] falha {}: {e}", s.name); None }
    }
}

fn main() {
    let pid1 = std::process::id() == 1;
    if pid1 { mount_early(); }
    install_signals();
    let dir = env::args().nth(1).unwrap_or("/etc/aegis/services".into());
    let profile = fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(mem_total_kb)
        .map(Profile::from_mem_kb).unwrap_or(Profile::UltraLight);
    eprintln!("[init] perfil de hardware: {profile:?}");

    let mut services: Vec<Service> = fs::read_dir(&dir).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "svc"))
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            parse_service(&name, &fs::read_to_string(&p).ok()?).map_err(|e| eprintln!("[init] {e}")).ok()
        })
        .filter(|s| profile >= s.min_profile)
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name)); // ordem determinística: prefixe 00-, 10-...

    let mut running: HashMap<i32, (usize, Instant)> = HashMap::new();
    for (i, s) in services.iter().enumerate() {
        if let Some(pid) = spawn(s) { running.insert(pid, (i, Instant::now())); }
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
        eprintln!("[init] {} saiu (status={status})", s.name);
        if !s.restart { continue; }
        // ponytail: backoff fixo 1s p/ crash-loop; exponencial se necessário
        if started.elapsed() < Duration::from_secs(1) { thread::sleep(Duration::from_secs(1)); }
        if let Some(p) = spawn(s) { running.insert(p, (i, Instant::now())); }
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
    }
}
