//! lange-compositor: compositor Wayland do LangeOS (Blueprint §3, Camada 3; etapa 7).
//!
//!   lange-compositor [--socket CAMINHO] [--size LxA] [--screenshot ARQ.ppm]
//!
//! Smithay cuida dos protocolos; a composição é feita na CPU num dumb buffer DRM (/dev/dri/card0).
//! Sem DRM roda headless (framebuffer só em memória). Sem Mesa/EGL/pixman no host: só Rust.
//! Protocolos: compositor+subcompositor, xdg_shell, shm, linux-dmabuf (linear), viewporter,
//! presentation, output. É o que o hwcomposer do Waydroid (container LangeDroid) usa.
use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, fs};

use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufMappingMode, DmabufSyncFlags};
use smithay::backend::allocator::{Buffer, Format, Fourcc, Modifier};
use smithay::reexports::drm;
use smithay::input::{touch, Seat, SeatHandler, SeatState};
use smithay::output::{Mode as OutMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat, wl_shm, wl_surface::WlSurface};
// wl_seat só com touch (sem teclado: libxkbcommon é stub, ver stub/).
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket, Resource};
use smithay::utils::{Serial, Transform, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_states, with_surface_tree_downward, with_surface_tree_upward, BufferAssignment, CompositorClientState, CompositorHandler,
    CompositorState, SubsurfaceCachedState, SurfaceAttributes, TraversalAction,
};
use smithay::wayland::dmabuf::{get_dmabuf, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::presentation::{PresentationFeedbackCachedState, PresentationState, Refresh};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::wayland::shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState};
use smithay::wayland::shm::{with_buffer_contents, ShmHandler, ShmState};
use smithay::wayland::viewporter::{ViewportCachedState, ViewporterState};

// ---------- composição na CPU (pura, testada) ----------

/// Imagem XRGB8888 (u32 0xAARRGGBB) com flag de alfa.
struct Image { w: usize, h: usize, px: Vec<u32>, alpha: bool }

/// Converte um pixel lido do cliente p/ 0xAARRGGBB. Formatos *BGR* vêm com R e B trocados.
fn to_argb(p: u32, swap_rb: bool, alpha: bool) -> u32 {
    let p = if swap_rb { (p & 0xff00_ff00) | ((p & 0xff) << 16) | ((p >> 16) & 0xff) } else { p };
    if alpha { p } else { p | 0xff00_0000 }
}

/// src-over com alfa não pré-multiplicado... Wayland usa pré-multiplicado: dst = src + dst*(1-a).
fn blend(src: u32, dst: u32) -> u32 {
    let a = src >> 24;
    if a == 255 { return src; }
    if a == 0 { return dst; }
    let inv = 255 - a;
    let ch = |s: u32, d: u32| (s + (d * inv + 127) / 255).min(255);
    let (sr, sg, sb) = ((src >> 16) & 0xff, (src >> 8) & 0xff, src & 0xff);
    let (dr, dg, db) = ((dst >> 16) & 0xff, (dst >> 8) & 0xff, dst & 0xff);
    0xff00_0000 | (ch(sr, dr) << 16) | (ch(sg, dg) << 8) | ch(sb, db)
}

/// Desenha `img` em `fb` (largura `fw`, altura `fh`, em pixels) na posição (x,y) com tamanho de
/// destino (dw,dh): escala vizinho-mais-próximo. ponytail: sem filtro/bilinear; trocar por GPU depois.
/// Os laços só percorrem a parte visível: tamanho de destino vem do cliente (viewporter) e não
/// pode virar trabalho ilimitado. Aritmética em i64 (sem overflow de x + dx).
fn draw(fb: &mut [u32], fw: usize, fh: usize, img: &Image, x: i32, y: i32, dw: usize, dh: usize) {
    if img.w == 0 || img.h == 0 || dw == 0 || dh == 0 { return; }
    let span = |pos: i32, len: usize, max: usize| -> (usize, usize) {
        let (pos, len, max) = (pos as i64, len.min(i64::MAX as usize) as i64, max as i64);
        let start = (-pos).clamp(0, len);
        let end = (max - pos).clamp(start, len);
        (start as usize, end as usize)
    };
    let (y0, y1) = span(y, dh, fh);
    let (x0, x1) = span(x, dw, fw);
    if x0 >= x1 { return; }
    let tx0 = (x as i64 + x0 as i64) as usize;
    // coluna de origem p/ cada coluna visível: calculada uma vez, não por pixel (CPU emulada é lenta)
    let cols: Vec<usize> = (x0..x1).map(|dx| (dx as u128 * img.w as u128 / dw as u128) as usize).collect();
    let same_w = dw == img.w;
    for dy in y0..y1 {
        let ty = (y as i64 + dy as i64) as usize;
        let sy = (dy as u128 * img.h as u128 / dh as u128) as usize;
        let row = &img.px[sy * img.w..(sy + 1) * img.w];
        let out = &mut fb[ty * fw + tx0..ty * fw + tx0 + cols.len()];
        match (img.alpha, same_w) {
            (false, true) => out.copy_from_slice(&row[x0..x1]), // caso comum: 1:1 opaco
            (false, false) => for (d, &c) in out.iter_mut().zip(&cols) { *d = row[c]; },
            (true, true) => for (d, &s) in out.iter_mut().zip(&row[x0..x1]) { *d = blend(s, *d); },
            (true, false) => for (d, &c) in out.iter_mut().zip(&cols) { *d = blend(row[c], *d); },
        }
    }
}

/// Lê pixels de memória do cliente (stride em bytes) p/ Image.
fn read_pixels(ptr: *const u8, len: usize, off: usize, w: usize, h: usize, stride: usize, swap_rb: bool, alpha: bool) -> Option<Image> {
    if w == 0 || h == 0 || stride < w * 4 || off + stride * (h - 1) + w * 4 > len { return None; }
    let mut px = vec![0u32; w * h];
    for (row, out) in px.chunks_exact_mut(w).enumerate() {
        // cópia da linha inteira (memcpy) e conversão no lugar: bem mais rápido que ler pixel a pixel
        unsafe { std::ptr::copy_nonoverlapping(ptr.add(off + row * stride), out.as_mut_ptr() as *mut u8, w * 4) };
        if swap_rb || !alpha { for p in out.iter_mut() { *p = to_argb(u32::from_le(*p), swap_rb, alpha); } }
    }
    Some(Image { w, h, px, alpha })
}

/// (troca R/B, tem alfa) por fourcc DRM. None = formato não suportado.
fn fourcc_info(f: Fourcc) -> Option<(bool, bool)> {
    match f {
        Fourcc::Argb8888 => Some((false, true)),
        Fourcc::Xrgb8888 => Some((false, false)),
        Fourcc::Abgr8888 => Some((true, true)),
        Fourcc::Xbgr8888 => Some((true, false)),
        _ => None,
    }
}
const FORMATS: [Fourcc; 4] = [Fourcc::Argb8888, Fourcc::Xrgb8888, Fourcc::Abgr8888, Fourcc::Xbgr8888];

fn import_shm(buf: &wl_buffer::WlBuffer) -> Option<Image> {
    with_buffer_contents(buf, |ptr, len, d| {
        let (swap, alpha) = match d.format {
            wl_shm::Format::Argb8888 => (false, true),
            wl_shm::Format::Xrgb8888 => (false, false),
            wl_shm::Format::Abgr8888 => (true, true),
            wl_shm::Format::Xbgr8888 => (true, false),
            _ => return None,
        };
        read_pixels(ptr, len, d.offset as usize, d.width as usize, d.height as usize, d.stride as usize, swap, alpha)
    }).ok().flatten()
}

// ---------- virtio-gpu (virgl): trazer o conteúdo da GPU do host antes de ler ----------

const FRAME: Duration = Duration::from_millis(33); // ~30 Hz

#[repr(C)]
struct PollFd { fd: i32, events: i16, revents: i16 }
const POLLIN: i16 = 1;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, n: std::ffi::c_ulong, timeout_ms: i32) -> i32;
    fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
    fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
}

/// CLOCK_MONOTONIC (o clock anunciado no wp_presentation).
fn monotonic() -> Duration {
    let mut ts = [0i64; 2];
    unsafe { clock_gettime(1, &mut ts) };
    Duration::new(ts[0] as u64, ts[1] as u32)
}

/// _IOWR('d', DRM_COMMAND_BASE + nr, size) — codificação genérica do Linux (arm64/x86).
const fn drm_iowr(nr: u64, size: u64) -> std::ffi::c_ulong { ((3 << 30) | (size << 16) | ((b'd' as u64) << 8) | (0x40 + nr)) as _ }
const VIRTGPU_TRANSFER_FROM_HOST: std::ffi::c_ulong = drm_iowr(0x06, 44);
const VIRTGPU_WAIT: std::ffi::c_ulong = drm_iowr(0x08, 8);

/// Com virgl o Android renderiza na GPU do host; a memória da guest só reflete o resultado após
/// TRANSFER_FROM_HOST (o que o driver virgl da Mesa faz ao mapear). Em GPU real o ioctl falha
/// (não é virtio) e nada muda. ponytail: transfere o quadro inteiro a cada commit.
/// Resultado p/ diagnóstico: Ok, ou o passo + erro que falhou.
fn virtio_readback(card: &Card, dmabuf: &Dmabuf, w: u32, h: u32) -> Result<(), String> {
    use drm::control::Device as _;
    let fd = dmabuf.handles().next().ok_or("sem plano")?;
    let handle = card.prime_fd_to_buffer(fd).map_err(|e| format!("prime import: {e}"))?;
    let bo: u32 = handle.into();
    // handle, box{x,y,z,w,h,d}, level, offset, stride, layer_stride. stride/layer_stride = 0: recurso
    // não-blob usa o stride do próprio recurso; o kernel devolve EINVAL se vier preenchido.
    let mut xfer = [bo, 0, 0, 0, w, h, 1, 0, 0, 0, 0u32];
    let mut wait = [bo, 0u32];
    let fd = card.0.as_fd().as_raw_fd();
    let res = unsafe {
        if ioctl(fd, VIRTGPU_TRANSFER_FROM_HOST, xfer.as_mut_ptr()) != 0 {
            Err(format!("transfer: {}", std::io::Error::last_os_error()))
        } else if ioctl(fd, VIRTGPU_WAIT, wait.as_mut_ptr()) != 0 {
            Err(format!("wait: {}", std::io::Error::last_os_error()))
        } else { Ok(()) }
    };
    let _ = card.close_buffer(handle);
    res
}

static INPUT_LOGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static READBACK_LOGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn import_dmabuf(dmabuf: &Dmabuf, card: Option<&Card>) -> Option<Image> {
    let (swap, alpha) = fourcc_info(dmabuf.format().code)?;
    let stride = dmabuf.strides().next()? as usize;
    let size = dmabuf.size();
    if let Some(card) = card {
        let r = virtio_readback(card, dmabuf, size.w as u32, size.h as u32);
        if READBACK_LOGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 { eprintln!("[compositor] readback virtio: {r:?}"); }
    }
    let _ = dmabuf.sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::READ);
    let map = dmabuf.map_plane(0, DmabufMappingMode::READ).ok()?;
    let img = read_pixels(map.ptr() as *const u8, map.length(), 0, size.w as usize, size.h as usize, stride, swap, alpha);
    let _ = dmabuf.sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::READ);
    img
}

// ---------- shell nativo (src/shell.rs) ----------

mod shell;
mod pin;
const PIN_DIR: &str = "/data/lange";
const PIN_FILE: &str = "/data/lange/pin";
const PIN_FAILS_FILE: &str = "/data/lange/pin-fails";
use shell::{Action, Canvas, Mode, Shell, BAR, NAV};

/// "HH:MM" UTC. ponytail: sem fuso horário (sem tzdata no rootfs); ler TZ quando houver config de usuário.
fn clock_now() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{:02}:{:02}", s / 3600 % 24, s / 60 % 60)
}

// ---------- input: evdev (toque / tablet absoluto) ----------

/// Evento de toque por dedo (slot 0..SLOTS).
#[derive(Debug, PartialEq, Clone, Copy)]
enum TouchEv { Down(usize, f64, f64), Motion(usize, f64, f64), Up(usize) }

const SLOTS: usize = 10;

#[derive(Clone, Copy, Default)]
struct Slot { id: i32, x: i32, y: i32, was: bool, moved: bool }

/// Dispositivo evdev absoluto: multitouch (protocolo B, ABS_MT_*: touchscreen real, virtio-multitouch)
/// ou single-touch (ABS_X/ABS_Y + BTN_TOUCH/BTN_LEFT: virtio-tablet), este como slot 0.
struct Tablet { file: fs::File, max: (i32, i32), mt: bool, slots: [Slot; SLOTS], cur: usize }

const EV_SYN: u16 = 0; const EV_KEY: u16 = 1; const EV_ABS: u16 = 3;
const BTN_LEFT: u16 = 0x110; const BTN_TOUCH: u16 = 0x14a;
const ABS_MT_SLOT: u16 = 0x2f; const ABS_MT_POSITION_X: u16 = 0x35; const ABS_MT_POSITION_Y: u16 = 0x36; const ABS_MT_TRACKING_ID: u16 = 0x39;

/// EVIOCGABS(abs) = _IOR('E', 0x40 + abs, struct input_absinfo /* 24 bytes */)
const fn eviocgabs(abs: u64) -> std::ffi::c_ulong { ((2 << 30) | (24 << 16) | ((b'E' as u64) << 8) | (0x40 + abs)) as _ }

impl Tablet {
    fn new(file: fs::File, max: (i32, i32), mt: bool) -> Tablet {
        Tablet { file, max, mt, slots: [Slot { id: -1, ..Default::default() }; SLOTS], cur: 0 }
    }

    /// Primeiro /dev/input/event* com eixos absolutos (MT preferido, senão X/Y).
    fn open() -> Option<Tablet> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut names: Vec<_> = fs::read_dir("/dev/input").ok()?.flatten().map(|e| e.path()).collect();
        names.sort();
        for path in names.into_iter().filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event"))) {
            let Ok(file) = fs::OpenOptions::new().read(true).custom_flags(0o4000 /* O_NONBLOCK */).open(&path) else { continue };
            let max = |abs: u16| { let mut info = [0i32; 6]; (unsafe { ioctl(file.as_raw_fd(), eviocgabs(abs as u64), info.as_mut_ptr()) } == 0 && info[2] > 0).then_some(info[2]) };
            let (mt, axes) = match (max(ABS_MT_POSITION_X), max(ABS_MT_POSITION_Y)) {
                (Some(x), Some(y)) => (true, (x, y)),
                _ => match (max(0), max(1)) { (Some(x), Some(y)) => (false, (x, y)), _ => continue },
            };
            eprintln!("[compositor] input {} (abs {}x{}, {})", path.display(), axes.0, axes.1, if mt { "multitouch" } else { "1 toque" });
            return Some(Tablet::new(file, axes, mt));
        }
        None
    }

    /// Lê o que houver (não bloqueia) e devolve eventos de toque em pixels da tela.
    fn read(&mut self, sw: usize, sh: usize) -> Vec<TouchEv> {
        use std::io::Read;
        let mut buf = [0u8; 24 * 64];
        let mut out = Vec::new();
        while let Ok(n) = self.file.read(&mut buf) && n > 0 {
            for ev in buf[..n].chunks_exact(24) { self.feed(ev, sw, sh, &mut out); }
        }
        // Coordenadas de toque revelam o que se digita (PIN, teclado): só as primeiras (diagnóstico do
        // boot), ou todas com LANGE_DEBUG_INPUT=1.
        let all = std::env::var_os("LANGE_DEBUG_INPUT").is_some();
        if !out.is_empty() && (all || INPUT_LOGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5) {
            eprintln!("[compositor] toque {out:?}");
        }
        out
    }

    /// struct input_event (64-bit): timeval(16) type(u16) code(u16) value(i32).
    fn feed(&mut self, ev: &[u8], sw: usize, sh: usize, out: &mut Vec<TouchEv>) {
        let ty = u16::from_le_bytes([ev[16], ev[17]]);
        let code = u16::from_le_bytes([ev[18], ev[19]]);
        let val = i32::from_le_bytes([ev[20], ev[21], ev[22], ev[23]]);
        let cur = self.cur;
        match (ty, code, self.mt) {
            (EV_ABS, ABS_MT_SLOT, true) => self.cur = (val.max(0) as usize).min(SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID, true) => self.slots[cur].id = val,
            (EV_ABS, ABS_MT_POSITION_X, true) | (EV_ABS, 0, false) => { self.slots[cur].x = val; self.slots[cur].moved = true; }
            (EV_ABS, ABS_MT_POSITION_Y, true) | (EV_ABS, 1, false) => { self.slots[cur].y = val; self.slots[cur].moved = true; }
            (EV_KEY, BTN_LEFT | BTN_TOUCH, false) => self.slots[0].id = if val != 0 { 0 } else { -1 },
            (EV_SYN, 0, _) => {
                for (i, s) in self.slots.iter_mut().enumerate() {
                    let x = s.x.clamp(0, self.max.0) as f64 * sw as f64 / (self.max.0 as f64 + 1.0);
                    let y = s.y.clamp(0, self.max.1) as f64 * sh as f64 / (self.max.1 as f64 + 1.0);
                    let down = s.id >= 0;
                    match (s.was, down) {
                        (false, true) => out.push(TouchEv::Down(i, x, y)),
                        (true, false) => out.push(TouchEv::Up(i)),
                        (true, true) if s.moved => out.push(TouchEv::Motion(i, x, y)),
                        _ => {}
                    }
                    s.was = down;
                    s.moved = false;
                }
            }
            _ => {}
        }
    }
}

// ---------- privilégios ----------

/// uid/gid próprio do compositor (fora da faixa do Android, >= 1000). O socket fica 600:1000 0660.
const COMPOSITOR_UID: u32 = 600;
const ANDROID_SYSTEM_GID: u32 = 1000;

unsafe extern "C" {
    fn setgroups(n: usize, list: *const u32) -> i32;
    fn setgid(g: u32) -> i32;
    fn setuid(u: u32) -> i32;
}

/// Larga root depois de abrir DRM, input e socket. Falha = aborta (não roda root por engano).
fn drop_root() {
    unsafe {
        if setgroups(0, std::ptr::null()) != 0 || setgid(COMPOSITOR_UID) != 0 || setuid(COMPOSITOR_UID) != 0 {
            eprintln!("[compositor] não consegui largar root: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
        if setuid(0) == 0 { eprintln!("[compositor] ainda root depois do setuid"); std::process::exit(1); }
    }
    eprintln!("[compositor] rodando como uid {COMPOSITOR_UID}");
}

// ---------- saída: DRM dumb buffer ou headless ----------

struct Card(fs::File);
impl AsFd for Card { fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_fd() } }
impl drm::Device for Card {}
impl drm::control::Device for Card {}

struct Screen { w: usize, h: usize, fb: Vec<u32>, drm: Option<(Card, drm::control::dumbbuffer::DumbBuffer, drm::control::framebuffer::Handle)> }

impl Screen {
    fn open(want: Option<(usize, usize)>) -> Screen {
        match Self::open_drm() {
            Ok(s) => s,
            Err(e) => {
                let (w, h) = want.unwrap_or((1280, 720));
                eprintln!("[compositor] sem DRM ({e}); headless {w}x{h}");
                Screen { w, h, fb: vec![0; w * h], drm: None }
            }
        }
    }

    fn open_drm() -> Result<Screen, String> {
        use drm::control::Device as _;
        let file = fs::OpenOptions::new().read(true).write(true).open("/dev/dri/card0").map_err(|e| format!("card0: {e}"))?;
        let card = Card(file);
        let res = card.resource_handles().map_err(|e| format!("resources: {e}"))?;
        let conn = res.connectors().iter()
            .filter_map(|&c| card.get_connector(c, false).ok())
            .find(|c| c.state() == drm::control::connector::State::Connected)
            .ok_or("nenhum conector conectado")?;
        let mode = *conn.modes().first().ok_or("conector sem modos")?;
        let crtc = *res.crtcs().first().ok_or("sem CRTC")?;
        let (w, h) = mode.size();
        let mut db = card.create_dumb_buffer((w as u32, h as u32), drm::buffer::DrmFourcc::Xrgb8888, 32).map_err(|e| format!("dumb: {e}"))?;
        let fb = card.add_framebuffer(&db, 24, 32).map_err(|e| format!("addfb: {e}"))?;
        card.set_crtc(crtc, Some(fb), (0, 0), &[conn.handle()], Some(mode)).map_err(|e| format!("set_crtc: {e}"))?;
        { let mut m = card.map_dumb_buffer(&mut db).map_err(|e| format!("map: {e}"))?; m.as_mut().fill(0); }
        eprintln!("[compositor] DRM card0 {w}x{h}");
        Ok(Screen { w: w as usize, h: h as usize, fb: vec![0; w as usize * h as usize], drm: Some((card, db, fb)) })
    }

    /// Copia o framebuffer da CPU p/ o dumb buffer (scanout). ponytail: buffer único (pode rasgar); page flip depois.
    fn present(&mut self) {
        use drm::buffer::Buffer as _;
        use drm::control::Device as _;
        let Some((card, db, fb)) = &mut self.drm else { return };
        let pitch = db.pitch() as usize;
        let Ok(mut m) = card.map_dumb_buffer(db) else { return };
        let out = m.as_mut();
        // fb é u32 nativo = XRGB8888 little-endian (arm64/x86): copia bytes direto
        let src = unsafe { std::slice::from_raw_parts(self.fb.as_ptr() as *const u8, self.fb.len() * 4) };
        for y in 0..self.h {
            out[y * pitch..y * pitch + self.w * 4].copy_from_slice(&src[y * self.w * 4..(y + 1) * self.w * 4]);
        }
        drop(m);
        // virtio-gpu (e outras GPUs sem scanout direto da memória) só atualizam a tela com DIRTYFB
        if let Err(e) = card.dirty_framebuffer(*fb, &[drm::control::ClipRect::new(0, 0, self.w as u16, self.h as u16)]) {
            static LOGGED: std::sync::Once = std::sync::Once::new();
            LOGGED.call_once(|| eprintln!("[compositor] dirtyfb: {e}"));
        }
    }

    fn screenshot(&self, path: &str) -> std::io::Result<()> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.fb { out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]); }
        let tmp = format!("{path}.tmp");
        let mut f = fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, &out)?;
        f.sync_all()?; // QEMU pode ser morto a qualquer hora: o teste lê o disco depois
        fs::rename(tmp, path) // atômico: leitor nunca vê PPM pela metade
    }
}

// ---------- estado Wayland ----------

struct App {
    compositor: CompositorState,
    xdg: XdgShellState,
    shm: ShmState,
    dmabuf: DmabufState,
    _dmabuf_global: Option<DmabufGlobal>,
    _viewporter: ViewporterState,
    _presentation: PresentationState,
    seat_state: SeatState<App>,
    output: Output,
    images: HashMap<ObjectId, Image>,
    /// buffers commitados ainda não copiados (1 por superfície)
    pending: HashMap<ObjectId, wl_buffer::WlBuffer>,
    dirty: bool,
    size: (i32, i32),
    commits: u32,
    /// fd DRM próprio (dup do card0) p/ o readback virtio; None em headless
    card: Option<Card>,
    seat: Seat<App>,
    touch_focus: Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)>,
    start: Instant,
    frame_seq: u64,
    import_time: Duration,
    shell: Shell,
    touch_shell: bool,
    /// dedos na tela agora e o slot do dedo que o shell acompanha
    fingers: usize,
    mt_logged: bool,
    shell_slot: usize,
    touch_pos: (f64, f64),
    /// pedidos p/ o langedroid (socket do shell), enviados pela thread `shell_client`
    requests: Vec<String>,
}

impl BufferHandler for App { fn buffer_destroyed(&mut self, _b: &wl_buffer::WlBuffer) {} }

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState { &mut self.compositor }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    /// Só guarda o buffer mais recente da superfície; a cópia (cara: readback virtio) acontece uma vez
    /// por quadro em import_pending(). Buffer substituído antes de ser desenhado volta ao cliente já.
    fn commit(&mut self, surface: &WlSurface) {
        let assignment = with_states(surface, |s| s.cached_state.get::<SurfaceAttributes>().current().buffer.take());
        match assignment {
            Some(BufferAssignment::NewBuffer(buf)) => {
                if let Some(old) = self.pending.insert(surface.id(), buf) { old.release(); }
            }
            Some(BufferAssignment::Removed) => {
                if let Some(old) = self.pending.remove(&surface.id()) { old.release(); }
                self.images.remove(&surface.id());
            }
            None => {} // posição de subsurface/viewport pode ter mudado
        }
        self.dirty = true;
    }
    fn destroyed(&mut self, surface: &WlSurface) {
        if let Some(old) = self.pending.remove(&surface.id()) { old.release(); }
        self.images.remove(&surface.id());
        self.dirty = true;
    }
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState { &mut self.xdg }
    /// Tela cheia: toda janela recebe o tamanho da saída (celular).
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let (w, h) = (self.size.0, self.size.1 - (BAR + NAV) as i32);
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(xdg_toplevel::State::Activated);
            s.states.set(xdg_toplevel::State::Fullscreen);
        });
        surface.send_configure();
        eprintln!("[compositor] nova janela");
    }
    fn new_popup(&mut self, _s: PopupSurface, _p: PositionerState) {}
    fn grab(&mut self, _s: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(&mut self, _s: PopupSurface, _p: PositionerState, _t: u32) {}
}

impl ShmHandler for App { fn shm_state(&self) -> &ShmState { &self.shm } }

impl DmabufHandler for App {
    fn dmabuf_state(&mut self) -> &mut DmabufState { &mut self.dmabuf }
    fn dmabuf_imported(&mut self, _g: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        // aceita só o que sabemos mapear e ler: 1 plano, formato conhecido
        let f = dmabuf.format();
        let ok = dmabuf.num_planes() == 1 && fourcc_info(f.code).is_some();
        eprintln!("[compositor] dmabuf {:?} modifier={:?} planos={} {}x{} -> {}", f.code, f.modifier,
            dmabuf.num_planes(), dmabuf.size().w, dmabuf.size().h, if ok { "aceito" } else { "RECUSADO" });
        if ok { let _ = notifier.successful::<App>(); } else { notifier.failed(); }
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<App> { &mut self.seat_state }
}

impl OutputHandler for App {}

smithay::delegate_compositor!(App);
smithay::delegate_xdg_shell!(App);
smithay::delegate_shm!(App);
smithay::delegate_dmabuf!(App);
smithay::delegate_output!(App);
smithay::delegate_viewporter!(App);
smithay::delegate_presentation!(App);
smithay::delegate_seat!(App);

#[derive(Default)]
struct ClientState { compositor: CompositorClientState }
impl ClientData for ClientState {
    fn initialized(&self, _id: ClientId) { eprintln!("[compositor] cliente conectado"); }
    fn disconnected(&self, _id: ClientId, _r: DisconnectReason) { eprintln!("[compositor] cliente saiu"); }
}

impl App {
    /// Copia os buffers pendentes p/ imagens e devolve os buffers aos clientes.
    fn import_pending(&mut self) {
        for (id, buf) in std::mem::take(&mut self.pending) {
            let kind = if get_dmabuf(&buf).is_ok() { "dmabuf" } else { "shm" };
            let img = match get_dmabuf(&buf) { Ok(d) => import_dmabuf(d, self.card.as_ref()), Err(_) => import_shm(&buf) };
            buf.release();
            // ponytail: log só das primeiras cópias (diagnóstico da etapa 7), sem inundar o console
            self.commits += 1;
            if self.commits <= 20 || self.commits % 100 == 0 {
                let lit = img.as_ref().map_or(0, |i| i.px.iter().filter(|&&p| p & 0x00ff_ffff != 0).count());
                eprintln!("[compositor] commit {kind} {:?} pixels_acesos={lit}", img.as_ref().map(|i| (i.w, i.h)));
            }
            match img {
                Some(img) => { self.images.insert(id, img); }
                None => eprintln!("[compositor] buffer com formato não suportado em {id:?}"),
            }
        }
    }

    /// Superfícies com conteúdo em ordem de desenho (fundo -> frente): (superfície, x, y, larg, alt)
    /// em coordenadas da tela. Janelas começam abaixo da barra de status.
    fn layers(&self) -> Vec<(WlSurface, i32, i32, usize, usize)> {
        let mut out = Vec::new();
        for top in self.xdg.toplevel_surfaces() {
            // upward = ordem de desenho. downward é a ordem de clique: com ele o fundo preto do
            // hwcomposer do Waydroid era desenhado por último e cobria tudo.
            with_surface_tree_upward(
                top.wl_surface(),
                (0, BAR as i32),
                |_, states, &(x, y)| {
                    let loc = states.cached_state.get::<SubsurfaceCachedState>().current().location;
                    TraversalAction::DoChildren((x + loc.x, y + loc.y))
                },
                |surf, states, &(x, y)| {
                    let loc = states.cached_state.get::<SubsurfaceCachedState>().current().location;
                    let Some(img) = self.images.get(&surf.id()) else { return };
                    let dst = states.cached_state.get::<ViewportCachedState>().current().dst;
                    let (dw, dh) = dst.map_or((img.w, img.h), |d| (d.w.max(0) as usize, d.h.max(0) as usize));
                    out.push((surf.clone(), x + loc.x, y + loc.y, dw, dh));
                },
                |_, _, _| true,
            );
        }
        out
    }

    /// Janela (toplevel) sob (px,py) e a origem dela na tela (foco do toque). O foco é sempre a
    /// superfície raiz: o hwcomposer do Waydroid repassa as coordenadas locais do wl_touch direto ao
    /// Android, como coordenadas da janela; com foco numa subsurface (ex.: barra de navegação) o
    /// toque caía no lugar errado.
    fn surface_at(&self, px: f64, py: f64) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        if self.shell.wants_touch() { return None; }
        let (w, h) = (self.size.0 as f64, (self.size.1 - (BAR + NAV) as i32) as f64);
        let (x, y) = (px, py - BAR as f64);
        if x < 0.0 || y < 0.0 || x >= w || y >= h { return None; }
        // ponytail: janelas em tela cheia empilhadas; a última criada fica na frente
        self.xdg.toplevel_surfaces().last().map(|t| (t.wl_surface().clone(), (0.0, BAR as f64).into()))
    }

    /// Compõe barra de status (nativa) + todas as janelas no framebuffer.
    fn render(&mut self, screen: &mut Screen) {
        let t = Instant::now();
        self.import_pending();
        self.import_time += t.elapsed();
        screen.fb.fill(0xff10_1418); // fundo LangeOS
        let (fw, fh) = (screen.w, screen.h);
        if self.shell.mode == Mode::App {
            // janela Android (painel rápido é desenhado por cima pelo shell) só entre as barras (ela é configurada com essa altura)
            let area = &mut screen.fb[..(fh - NAV) * fw];
            for (surf, x, y, dw, dh) in self.layers() {
                if let Some(img) = self.images.get(&surf.id()) { draw(area, fw, fh - NAV, img, x, y, dw, dh); }
            }
        }
        self.shell.draw(&mut Canvas { px: &mut screen.fb, w: fw, h: fh }, &clock_now(), shell::battery());
        // ponytail: brilho por software (escurece o quadro); /sys/class/backlight no aparelho real
        let b = self.shell.brightness as u32;
        if b < 100 {
            for p in screen.fb.iter_mut() {
                let ch = |s: u32| ((*p >> s & 0xff) * b / 100) << s;
                *p = 0xff00_0000 | ch(16) | ch(8) | ch(0);
            }
        }
    }

    /// Toque: barras e tela inicial/recentes são do shell; o resto vai p/ a janela Android.
    /// Quem recebe é decidido no `down` e vale até o `up`.
    /// Destino decidido pelo 1º dedo; os outros seguem o mesmo (Android recebe todos; shell só o 1º).
    fn touch(&mut self, ev: TouchEv) {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let first = self.fingers == 0;
        match ev {
            TouchEv::Down(..) => self.fingers += 1,
            TouchEv::Up(_) => self.fingers = self.fingers.saturating_sub(1),
            _ => {}
        }
        if let TouchEv::Down(slot, x, y) = ev && first {
            self.touch_shell = self.surface_at(x, y).is_none();
            self.shell_slot = slot;
            if self.touch_shell { self.shell.touch_down(x, y); }
            self.touch_pos = (x, y);
        }
        if !self.touch_shell { return self.touch_android(ev); }
        if let TouchEv::Motion(s, x, y) = ev && s == self.shell_slot { self.touch_pos = (x, y); }
        if ev == TouchEv::Up(self.shell_slot) {
            let (x, y) = self.touch_pos;
            let action = self.shell.touch_up(x, y, w, h);
            if action != Action::None { eprintln!("[compositor] shell {action:?} (modo {:?}, painel {})", self.shell.mode, self.shell.panel); }
            match action {
                Action::None => {}
                Action::Redraw => self.dirty = true,
                Action::Launch(c) => { self.requests.push(format!("launch {c}")); self.dirty = true; }
                Action::Key(k) => { self.requests.push(format!("key {k}")); self.dirty = true; }
                Action::Notifs => { self.requests.push("notifs".into()); self.dirty = true; }
                Action::SavePin => {
                    let r = self.shell.pin.as_ref().map(|p| p.save(PIN_FILE));
                    eprintln!("[compositor] PIN {}", match r { Some(Ok(())) => "salvo".to_string(), Some(Err(e)) => format!("não salvo: {e}"), None => "?".into() });
                    self.dirty = true;
                }
                Action::Volume(v) => { self.requests.push(format!("volume {v}")); self.dirty = true; }
            }
        }
    }

    /// Evento de toque p/ a janela Android (coordenadas da tela). Foco fixado no `down`, como no Wayland.
    fn touch_android(&mut self, ev: TouchEv) {
        let Some(t) = self.seat.get_touch() else { return };
        let time = self.start.elapsed().as_millis() as u32;
        // id = slot do dedo; nunca None: None vira -1 no wl_touch, e o hwcomposer repassa como
        // ABS_MT_TRACKING_ID -1 (= dedo solto)
        let id = |s: usize| smithay::backend::input::TouchSlot::from(Some(s as u32));
        match ev {
            TouchEv::Down(s, x, y) => {
                let slot = id(s);
                if self.fingers == 2 && !self.mt_logged { self.mt_logged = true; eprintln!("[compositor] multitouch: 2 dedos -> Android"); }
                // 2º dedo em diante: mesma janela do 1º (coordenadas locais dela)
                let focus = if self.fingers > 1 && self.touch_focus.is_some() { self.touch_focus.clone() } else { self.surface_at(x, y) };
                self.touch_focus = focus.clone();
                t.down(self, focus, &touch::DownEvent { slot, location: (x, y).into(), serial: SERIAL_COUNTER.next_serial(), time });
            }
            TouchEv::Motion(s, x, y) => {
                let (slot, focus) = (id(s), self.touch_focus.clone());
                t.motion(self, focus, &touch::MotionEvent { slot, location: (x, y).into(), time });
            }
            TouchEv::Up(s) => {
                let slot = id(s);
                if self.fingers == 0 { self.touch_focus = None; }
                t.up(self, &touch::UpEvent { slot, serial: SERIAL_COUNTER.next_serial(), time });
            }
        }
        t.frame(self);
    }

    /// frame callbacks + wp_presentation "presented". Sem o presented o hwcomposer do Waydroid não
    /// libera o present fence e o SurfaceFlinger para de mandar quadros (Android congelava na tela).
    fn send_frames(&mut self, time_ms: u32) {
        let now = monotonic();
        self.frame_seq += 1;
        let (output, seq) = (&self.output, self.frame_seq);
        for top in self.xdg.toplevel_surfaces() {
            with_surface_tree_downward(
                top.wl_surface(), (),
                |_, _, _| TraversalAction::DoChildren(()),
                |_, states, _| {
                    for cb in states.cached_state.get::<SurfaceAttributes>().current().frame_callbacks.drain(..) { cb.done(time_ms); }
                    let fbs = std::mem::take(&mut states.cached_state.get::<PresentationFeedbackCachedState>().current().callbacks);
                    for fb in fbs { fb.presented(output, now, Refresh::Fixed(FRAME), seq, wp_presentation_feedback::Kind::empty()); }
                },
                |_, _, _| true,
            );
        }
    }
}

/// Socket do shell no langedroid (só uid 600 = este compositor).
const SHELL_SOCK: &str = "/run/langedroid-shell.sock";

/// Thread que fala com o langedroid: comandos podem levar segundos (container lento no QEMU) e o
/// loop do compositor não pode parar. Uma conexão por pedido; resposta = (pedido, linha).
/// tmp + fsync + rename: queda de energia não deixa arquivo pela metade.
fn write_atomic(path: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = format!("{path}.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

fn shell_client() -> (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<(String, String)>) {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<String>();
    let (resp_tx, resp_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        for req in req_rx {
            let reply = (|| -> std::io::Result<String> {
                let s = std::os::unix::net::UnixStream::connect(SHELL_SOCK)?;
                s.set_read_timeout(Some(Duration::from_secs(60)))?;
                writeln!(&s, "{req}")?;
                let mut line = String::new();
                std::io::BufReader::new(&s).read_line(&mut line)?;
                Ok(line.trim().to_string())
            })().unwrap_or_else(|e| format!("erro {e}"));
            if resp_tx.send((req, reply)).is_err() { break; }
        }
    });
    (req_tx, resp_rx)
}

fn parse_size(s: &str) -> Option<(usize, usize)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn main() {
    let mut socket = "/run/lange/wayland-0".to_string();
    let mut want = None;
    let mut shot: Option<String> = None;
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" => socket = it.next().expect("--socket CAMINHO"),
            "--size" => want = Some(parse_size(&it.next().unwrap_or_default()).expect("--size LxA")),
            "--screenshot" => shot = it.next(),
            o => { eprintln!("uso: lange-compositor [--socket CAMINHO] [--size LxA] [--screenshot ARQ.ppm] ({o}?)"); std::process::exit(2) }
        }
    }

    let mut screen = Screen::open(want);
    let mut display: Display<App> = Display::new().expect("display");
    let dh = display.handle();

    let output = Output::new("LANGE-1".into(), PhysicalProperties {
        size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "LangeOS".into(), model: "virtual".into(),
    });
    let mode = OutMode { size: (screen.w as i32, screen.h as i32).into(), refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    output.create_global::<App>(&dh);

    let mut dmabuf = DmabufState::new();
    let formats = FORMATS.iter().map(|&code| Format { code, modifier: Modifier::Linear });
    let dmabuf_global = dmabuf.create_global::<App>(&dh, formats);

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "seat0");
    seat.add_touch();
    let mut app = App {
        compositor: CompositorState::new::<App>(&dh),
        xdg: XdgShellState::new::<App>(&dh),
        shm: ShmState::new::<App>(&dh, vec![wl_shm::Format::Abgr8888, wl_shm::Format::Xbgr8888]),
        dmabuf,
        _dmabuf_global: Some(dmabuf_global),
        _viewporter: ViewporterState::new::<App>(&dh),
        _presentation: PresentationState::new::<App>(&dh, 1 /* CLOCK_MONOTONIC */),
        seat_state,
        output,
        images: HashMap::new(),
        pending: HashMap::new(),
        dirty: true,
        size: (screen.w as i32, screen.h as i32),
        commits: 0,
        card: screen.drm.as_ref().and_then(|(c, _, _)| c.0.try_clone().ok()).map(Card),
        seat,
        touch_focus: None,
        start: Instant::now(),
        frame_seq: 0,
        import_time: Duration::ZERO,
        shell: Shell::new(),
        touch_shell: false, fingers: 0, mt_logged: false, shell_slot: 0,
        touch_pos: (0.0, 0.0),
        requests: Vec::new(),
    };
    
    if let Some(dir) = std::path::Path::new(&socket).parent() { let _ = fs::create_dir_all(dir); }
    let _ = fs::remove_file(&socket);
    let listener = ListeningSocket::bind_absolute(socket.clone().into()).expect("bind socket wayland");
    // Dono = compositor; grupo 1000 (system do Android no container; sem user ns os ids coincidem).
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::os::unix::fs::chown(&socket, Some(COMPOSITOR_UID), Some(ANDROID_SYSTEM_GID))
        .and_then(|_| fs::set_permissions(&socket, fs::Permissions::from_mode(0o660))) {
        eprintln!("[compositor] permissões do socket: {e}");
        std::process::exit(1);
    }
    // screenshot (diagnóstico) é escrito por tmp+rename: o diretório precisa ser nosso
    if let Some(dir) = shot.as_deref().and_then(|p| std::path::Path::new(p).parent()) {
        let _ = fs::create_dir_all(dir);
        let _ = std::os::unix::fs::chown(dir, Some(COMPOSITOR_UID), Some(COMPOSITOR_UID));
    }
    // PIN da tela de bloqueio: dir só do compositor (0700)
    let _ = fs::create_dir_all(PIN_DIR);
    let _ = std::os::unix::fs::chown(PIN_DIR, Some(COMPOSITOR_UID), Some(COMPOSITOR_UID));
    let _ = fs::set_permissions(PIN_DIR, fs::Permissions::from_mode(0o700));
    app.shell.pin = pin::Pin::load(PIN_FILE);
    // erros de PIN sobrevivem a reinício do compositor (senão reiniciar zerava a espera)
    // sem arquivo = 0; arquivo ilegível = falha fechada (uma leva de erros: espera ativa)
    app.shell.set_pin_fails(match fs::read_to_string(PIN_FAILS_FILE) {
        Ok(t) => t.trim().parse().unwrap_or(shell::PIN_TRIES),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => shell::PIN_TRIES,
    });
    let mut saved_fails = app.shell.pin_fails();
    let mut tablet = Tablet::open();
    if tablet.is_none() { eprintln!("[compositor] sem dispositivo de toque"); }
    drop_root();
    eprintln!("[compositor] pronto em {socket} ({}x{})", screen.w, screen.h);

    let mut minute = clock_now();
    let (req_tx, resp_rx) = shell_client();
    // lista de apps: pede até o Android responder (boot leva minutos no QEMU)
    let (mut apps_asked, mut apps_pending) = (Instant::now() - Duration::from_secs(60), false);
    let (mut icons_asked, mut icons_pending) = (Instant::now() - Duration::from_secs(60), false);
    // diagnóstico do toque (android/test-android.sh): quadro antes do 1º toque e ~4 s após soltar
    let (mut pre_touch_saved, mut post_touch_at) = (false, None::<Instant>);
    // estatística a cada 10 s: commits recebidos, quadros desenhados, tempo médio de render
    let (mut stat_at, mut renders, mut render_time, mut last_commits) = (Instant::now(), 0u32, Duration::ZERO, 0u32);
    let mut last_shot = Instant::now() - Duration::from_secs(10);
    let mut last_frame = Instant::now() - FRAME;
    let mut best_lit = 0usize; // quadro mais "aceso" já visto -> <screenshot>-max.ppm (diagnóstico)
    let mut clients = Vec::new();
    loop {
        // Acorda quando um cliente fala (o socket dele não enche) ou no próximo quadro.
        let until_frame = FRAME.saturating_sub(last_frame.elapsed()).as_millis() as i32;
        let mut fds = [
            PollFd { fd: listener.as_raw_fd(), events: POLLIN, revents: 0 },
            PollFd { fd: display.backend().poll_fd().as_raw_fd(), events: POLLIN, revents: 0 },
            PollFd { fd: tablet.as_ref().map_or(-1, |t| t.file.as_raw_fd()), events: POLLIN, revents: 0 },
        ];
        unsafe { poll(fds.as_mut_ptr(), fds.len() as _, if app.dirty { until_frame.max(1) } else { 200 }) };

        while let Ok(Some(stream)) = listener.accept() {
            if let Ok(c) = display.handle().insert_client(stream, Arc::new(ClientState::default())) { clients.push(c); }
        }
        let _ = display.dispatch_clients(&mut app);
        if let Some(t) = &mut tablet {
            for ev in t.read(screen.w, screen.h) {
                if let Some(p) = &shot {
                    if matches!(ev, TouchEv::Down(..)) && !pre_touch_saved {
                        pre_touch_saved = true;
                        let _ = screen.screenshot(&p.replace(".ppm", "-pre-touch.ppm"));
                    }
                    // QEMU: Android reage devagar
                    if matches!(ev, TouchEv::Up(_)) { post_touch_at = Some(Instant::now() + Duration::from_secs(20)); }
                }
                app.touch(ev);
                if app.shell.pin_fails() != saved_fails {
                    saved_fails = app.shell.pin_fails();
                    if let Err(e) = write_atomic(PIN_FAILS_FILE, &saved_fails.to_string()) { eprintln!("[compositor] erros de PIN: {e}"); }
                }
            }
        }
        if clock_now() != minute { minute = clock_now(); app.dirty = true; }
        // vazia: a cada 15 s; depois a cada 60 s (no boot só aparecem apps direct-boot; instalações novas)
        let every = if app.shell.apps.is_empty() { 15 } else { 60 };
        if !apps_pending && apps_asked.elapsed() >= Duration::from_secs(every) {
            apps_asked = Instant::now();
            apps_pending = req_tx.send("apps".into()).is_ok();
            let _ = req_tx.send("net".into());
        }
        // ícones reais: um pedido por vez, no máximo a cada 30 s (erro = tenta de novo)
        if !icons_pending && icons_asked.elapsed() >= Duration::from_secs(30) && let Some(r) = (!apps_pending).then(|| shell::icons_request(&app.shell)).flatten() {
            icons_asked = Instant::now();
            icons_pending = req_tx.send(r).is_ok();
        }
        for r in app.requests.drain(..) { let _ = req_tx.send(r); }
        while let Ok((req, reply)) = resp_rx.try_recv() {
            if req == "apps" {
                apps_pending = false;
                match shell::parse_apps(&reply) {
                    Some(apps) if !apps.is_empty() && apps != app.shell.apps => {
                        eprintln!("[compositor] {} apps", apps.len());
                        app.shell.apps = apps;
                        // posição de cada ícone (testes automáticos tocam por aqui)
                        for (i, (x, y, w, h)) in app.shell.grid(screen.w, screen.h) {
                            eprintln!("[compositor] icone {} {} {}", app.shell.apps[i].label, x + w / 2, y + h / 2);
                        }
                        app.dirty = true;
                    }
                    Some(_) => {}
                    None => eprintln!("[compositor] apps: {}", reply.chars().take(120).collect::<String>()),
                }
            } else if req.starts_with("icons ") {
                icons_pending = false;
                let got = shell::parse_icons(&reply);
                eprintln!("[compositor] {} ícones do Android", got.len());
                // app sem ícone na resposta fica vazio (= ícone de letra) p/ não pedir de novo
                if reply.starts_with("ok") { for c in req.split(' ').skip(2) { app.shell.icons.entry(c.to_string()).or_default(); } }
                for (c, px) in got { app.shell.icons.insert(c, px); }
                app.dirty = true;
            } else if req == "net" {
                let net = reply.strip_prefix("ok").map(|i| i.trim().to_string());
                if net.is_some() && net != app.shell.net { app.shell.net = net; app.dirty = true; }
            } else if req == "notifs" {
                if !reply.starts_with("ok") { eprintln!("[compositor] notifs: {reply}"); }
                if let Some(n) = shell::parse_notifs(&reply) { app.shell.notifs = n; app.dirty = true; }
            } else if req == "key back" && reply == "ok home" && app.shell.mode == Mode::App && app.shell.pin_step.is_none() {
                // resposta assíncrona: só sai do app; nunca tira da tela de bloqueio (Bloquear pode ter vindo antes)
                app.shell.mode = Mode::Home; // saiu do app: tela inicial nativa, não a launcher do Android
                app.dirty = true;
            } else if reply.starts_with("erro") {
                eprintln!("[compositor] {req}: {reply}");
            }
        }
        // ponytail: sem vblank; quadro no máximo a cada FRAME (limita o custo do readback no QEMU)
        if app.dirty && last_frame.elapsed() >= FRAME {
            app.dirty = false;
            last_frame = Instant::now();
            let t = Instant::now();
            app.render(&mut screen);
            screen.present();
            renders += 1;
            render_time += t.elapsed();
            app.send_frames(app.start.elapsed().as_millis() as u32);
        }
        let _ = display.flush_clients();
        if stat_at.elapsed() >= Duration::from_secs(10) {
            eprintln!("[compositor] 10s: commits={} quadros={} render_médio={:?} (import {:?})", app.commits - last_commits, renders,
                render_time.checked_div(renders.max(1)).unwrap_or_default(), app.import_time.checked_div(renders.max(1)).unwrap_or_default());
            app.import_time = Duration::ZERO;
            (stat_at, renders, render_time, last_commits) = (Instant::now(), 0, Duration::ZERO, app.commits);
        }
        if let (Some(p), Some(at)) = (&shot, post_touch_at) && Instant::now() >= at {
            post_touch_at = None;
            let _ = screen.screenshot(&p.replace(".ppm", "-post-touch.ppm"));
        }
        if let Some(p) = &shot && last_shot.elapsed() >= Duration::from_secs(2) {
            last_shot = Instant::now();
            if let Err(e) = screen.screenshot(p) { eprintln!("[compositor] screenshot: {e}"); }
            let lit = screen.fb.iter().filter(|&&px| px & 0x00ff_ffff != 0 && px != 0xff10_1418).count();
            if lit > best_lit {
                best_lit = lit;
                let _ = screen.screenshot(&p.replace(".ppm", "-max.ppm"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pixels() {
        assert_eq!(to_argb(0x00_11_22_33, false, false), 0xff_11_22_33); // X -> opaco
        assert_eq!(to_argb(0x80_11_22_33, true, true), 0x80_33_22_11);   // ABGR -> ARGB
        assert_eq!(blend(0xff_10_20_30, 0xff_ff_ff_ff), 0xff_10_20_30);
        assert_eq!(blend(0x00_00_00_00, 0xff_12_34_56), 0xff_12_34_56);
        // pré-multiplicado: metade preto sobre branco ~ cinza 50%
        assert_eq!(blend(0x80_00_00_00, 0xff_ff_ff_ff), 0xff_7f_7f_7f);
    }
    #[test]
    fn draw_scales_and_clips() {
        let img = Image { w: 2, h: 1, px: vec![0xff_00_00_01, 0xff_00_00_02], alpha: false };
        let mut fb = vec![0u32; 4 * 2];
        draw(&mut fb, 4, 2, &img, 0, 0, 4, 2); // 2x1 -> 4x2
        assert_eq!(fb, [1, 1, 2, 2, 1, 1, 2, 2].map(|v| 0xff00_0000 | v));
        let mut fb = vec![0u32; 4];
        draw(&mut fb, 2, 2, &img, 1, 1, 2, 1); // recorta fora da tela
        assert_eq!(fb, [0, 0, 0, 0xff_00_00_01]);
        // destino gigante (cliente malicioso): termina rápido, só toca a área visível
        let t = std::time::Instant::now();
        let mut fb = vec![0u32; 4];
        draw(&mut fb, 2, 2, &img, -1_000_000, i32::MAX - 1, usize::MAX / 2, usize::MAX / 2);
        draw(&mut fb, 2, 2, &img, i32::MIN, i32::MIN, usize::MAX, usize::MAX);
        assert!(t.elapsed() < std::time::Duration::from_millis(100));
    }
    #[test]
    fn read_bounds() {
        let data = [0u8; 16];
        assert!(read_pixels(data.as_ptr(), 16, 0, 2, 2, 8, false, false).is_some());
        assert!(read_pixels(data.as_ptr(), 16, 4, 2, 2, 8, false, false).is_none()); // estoura
        assert!(read_pixels(data.as_ptr(), 16, 0, 2, 2, 4, false, false).is_none()); // stride < w*4
    }
    #[test]
    fn virtgpu_ioctls() {
        // conferidos com include/uapi/drm/virtgpu_drm.h (structs de 44 e 8 bytes)
        assert_eq!(VIRTGPU_TRANSFER_FROM_HOST, 0xc02c6446);
        assert_eq!(VIRTGPU_WAIT, 0xc0086448);
        assert_eq!(eviocgabs(0), 0x80184540); // EVIOCGABS(ABS_X), linux/input.h
    }
    #[test]
    fn tablet_events() {
        let ev = |ty: u16, code: u16, val: i32| { let mut b = [0u8; 24]; b[16..18].copy_from_slice(&ty.to_le_bytes());
            b[18..20].copy_from_slice(&code.to_le_bytes()); b[20..24].copy_from_slice(&val.to_le_bytes()); b };
        let mut t = Tablet::new(fs::File::open("/dev/null").unwrap(), (32767, 32767), false);
        let mut out = Vec::new();
        for e in [ev(EV_ABS, 0, 16384), ev(EV_ABS, 1, 0), ev(EV_KEY, BTN_LEFT, 1), ev(EV_SYN, 0, 0),
                  ev(EV_ABS, 1, 32767), ev(EV_SYN, 0, 0), ev(EV_KEY, BTN_LEFT, 0), ev(EV_SYN, 0, 0),
                  ev(EV_ABS, 0, 1), ev(EV_SYN, 0, 0)] { t.feed(&e, 720, 1280, &mut out); }
        assert_eq!(out, [TouchEv::Down(0, 360.0, 0.0), TouchEv::Motion(0, 360.0, 1279.9609375), TouchEv::Up(0)]); // movimento sem toque: nada
        // multitouch: dois dedos, solta o 2º, depois o 1º
        let mut t = Tablet::new(fs::File::open("/dev/null").unwrap(), (999, 999), true);
        let mut out = Vec::new();
        for e in [ev(EV_ABS, ABS_MT_SLOT, 0), ev(EV_ABS, ABS_MT_TRACKING_ID, 7), ev(EV_ABS, ABS_MT_POSITION_X, 0), ev(EV_ABS, ABS_MT_POSITION_Y, 0),
                  ev(EV_ABS, ABS_MT_SLOT, 1), ev(EV_ABS, ABS_MT_TRACKING_ID, 8), ev(EV_ABS, ABS_MT_POSITION_X, 500), ev(EV_ABS, ABS_MT_POSITION_Y, 500), ev(EV_SYN, 0, 0),
                  ev(EV_ABS, ABS_MT_POSITION_X, 600), ev(EV_SYN, 0, 0),
                  ev(EV_ABS, ABS_MT_TRACKING_ID, -1), ev(EV_SYN, 0, 0),
                  ev(EV_ABS, ABS_MT_SLOT, 0), ev(EV_ABS, ABS_MT_TRACKING_ID, -1), ev(EV_SYN, 0, 0)] { t.feed(&e, 1000, 1000, &mut out); }
        assert_eq!(out, [TouchEv::Down(0, 0.0, 0.0), TouchEv::Down(1, 500.0, 500.0), TouchEv::Motion(1, 600.0, 500.0), TouchEv::Up(1), TouchEv::Up(0)]);
    }
    #[test]
    fn size_arg() { assert_eq!(parse_size("720x1280"), Some((720, 1280))); assert_eq!(parse_size("x"), None); }
}
