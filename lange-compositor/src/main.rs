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
use smithay::input::{SeatHandler, SeatState};
use smithay::output::{Mode as OutMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat, wl_shm, wl_surface::WlSurface};
// ponytail: sem wl_seat global nem teclado (input entra com o touch). SeatHandler só p/ xdg_shell;
// libxkbcommon.a vazia (stub/) satisfaz o -lxkbcommon do crate sem linkar código C.
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket, Resource};
use smithay::utils::{Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_states, with_surface_tree_downward, with_surface_tree_upward, BufferAssignment, CompositorClientState, CompositorHandler,
    CompositorState, SubsurfaceCachedState, SurfaceAttributes, TraversalAction,
};
use smithay::wayland::dmabuf::{get_dmabuf, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::presentation::PresentationState;
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
    for dy in y0..y1 {
        let ty = (y as i64 + dy as i64) as usize;
        let sy = (dy as u128 * img.h as u128 / dh as u128) as usize;
        let row = &img.px[sy * img.w..(sy + 1) * img.w];
        let out = &mut fb[ty * fw..(ty + 1) * fw];
        for dx in x0..x1 {
            let s = row[(dx as u128 * img.w as u128 / dw as u128) as usize];
            let d = &mut out[(x as i64 + dx as i64) as usize];
            *d = if img.alpha { blend(s, *d) } else { s };
        }
    }
}

/// Lê pixels de memória do cliente (stride em bytes) p/ Image.
fn read_pixels(ptr: *const u8, len: usize, off: usize, w: usize, h: usize, stride: usize, swap_rb: bool, alpha: bool) -> Option<Image> {
    if w == 0 || h == 0 || stride < w * 4 || off + stride * (h - 1) + w * 4 > len { return None; }
    let mut px = Vec::with_capacity(w * h);
    for row in 0..h {
        for col in 0..w {
            let p = unsafe { (ptr.add(off + row * stride + col * 4) as *const u32).read_unaligned() };
            px.push(to_argb(p, swap_rb, alpha));
        }
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

// ---------- saída: DRM dumb buffer ou headless ----------

struct Card(fs::File);
impl AsFd for Card { fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_fd() } }
impl drm::Device for Card {}
impl drm::control::Device for Card {}

struct Screen { w: usize, h: usize, fb: Vec<u32>, drm: Option<(Card, drm::control::dumbbuffer::DumbBuffer)> }

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
        Ok(Screen { w: w as usize, h: h as usize, fb: vec![0; w as usize * h as usize], drm: Some((card, db)) })
    }

    /// Copia o framebuffer da CPU p/ o dumb buffer (scanout). ponytail: buffer único (pode rasgar); page flip depois.
    fn present(&mut self) {
        use drm::buffer::Buffer as _;
        use drm::control::Device as _;
        let Some((card, db)) = &mut self.drm else { return };
        let pitch = db.pitch() as usize;
        let Ok(mut m) = card.map_dumb_buffer(db) else { return };
        let out = m.as_mut();
        for y in 0..self.h {
            let row = &self.fb[y * self.w..(y + 1) * self.w];
            let dst = &mut out[y * pitch..y * pitch + self.w * 4];
            for (d, s) in dst.chunks_exact_mut(4).zip(row) { d.copy_from_slice(&s.to_le_bytes()); }
        }
    }

    fn screenshot(&self, path: &str) -> std::io::Result<()> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.fb { out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]); }
        let tmp = format!("{path}.tmp");
        fs::write(&tmp, out)?;
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
        let (w, h) = self.size;
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

    /// Compõe todas as janelas (e subsurfaces) no framebuffer.
    fn render(&mut self, screen: &mut Screen) {
        self.import_pending();
        screen.fb.fill(0xff10_1418); // fundo LangeOS
        let (fw, fh) = (screen.w, screen.h);
        let images = &self.images;
        for top in self.xdg.toplevel_surfaces() {
            // upward = ordem de desenho (fundo -> frente). downward é a ordem de clique: com ele o
            // fundo preto do hwcomposer do Waydroid era desenhado por último e cobria tudo.
            with_surface_tree_upward(
                top.wl_surface(),
                (0, 0),
                |_, states, &(x, y)| {
                    let loc = states.cached_state.get::<SubsurfaceCachedState>().current().location;
                    TraversalAction::DoChildren((x + loc.x, y + loc.y))
                },
                |surf, states, &(x, y)| {
                    let loc = states.cached_state.get::<SubsurfaceCachedState>().current().location;
                    let (x, y) = (x + loc.x, y + loc.y);
                    let Some(img) = images.get(&surf.id()) else { return };
                    let dst = states.cached_state.get::<ViewportCachedState>().current().dst;
                    let (dw, dh) = dst.map_or((img.w, img.h), |d| (d.w.max(0) as usize, d.h.max(0) as usize));
                    draw(&mut screen.fb, fw, fh, img, x, y, dw, dh);
                },
                |_, _, _| true,
            );
        }
    }

    fn send_frames(&mut self, time_ms: u32) {
        for top in self.xdg.toplevel_surfaces() {
            with_surface_tree_downward(
                top.wl_surface(), (),
                |_, _, _| TraversalAction::DoChildren(()),
                |_, states, _| {
                    for cb in states.cached_state.get::<SurfaceAttributes>().current().frame_callbacks.drain(..) { cb.done(time_ms); }
                },
                |_, _, _| true,
            );
        }
    }
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

    let mut app = App {
        compositor: CompositorState::new::<App>(&dh),
        xdg: XdgShellState::new::<App>(&dh),
        shm: ShmState::new::<App>(&dh, vec![wl_shm::Format::Abgr8888, wl_shm::Format::Xbgr8888]),
        dmabuf,
        _dmabuf_global: Some(dmabuf_global),
        _viewporter: ViewporterState::new::<App>(&dh),
        _presentation: PresentationState::new::<App>(&dh, 1 /* CLOCK_MONOTONIC */),
        seat_state: SeatState::new(),
        output,
        images: HashMap::new(),
        pending: HashMap::new(),
        dirty: true,
        size: (screen.w as i32, screen.h as i32),
        commits: 0,
        card: screen.drm.as_ref().and_then(|(c, _)| c.0.try_clone().ok()).map(Card),
    };
    let _ = &app.output;

    if let Some(dir) = std::path::Path::new(&socket).parent() { let _ = fs::create_dir_all(dir); }
    let _ = fs::remove_file(&socket);
    let listener = ListeningSocket::bind_absolute(socket.clone().into()).expect("bind socket wayland");
    // Só o uid/gid 1000 (system do Android no container; sem user ns os uids coincidem) conecta.
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::os::unix::fs::chown(&socket, Some(1000), Some(1000))
        .and_then(|_| fs::set_permissions(&socket, fs::Permissions::from_mode(0o660))) {
        eprintln!("[compositor] permissões do socket: {e}");
        std::process::exit(1);
    }
    eprintln!("[compositor] pronto em {socket} ({}x{})", screen.w, screen.h);

    let start = Instant::now();
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
        ];
        unsafe { poll(fds.as_mut_ptr(), fds.len() as _, if app.dirty { until_frame.max(1) } else { 1000 }) };

        while let Ok(Some(stream)) = listener.accept() {
            if let Ok(c) = display.handle().insert_client(stream, Arc::new(ClientState::default())) { clients.push(c); }
        }
        let _ = display.dispatch_clients(&mut app);
        // ponytail: sem vblank; quadro no máximo a cada FRAME (limita o custo do readback no QEMU)
        if app.dirty && last_frame.elapsed() >= FRAME {
            app.dirty = false;
            last_frame = Instant::now();
            app.render(&mut screen);
            screen.present();
            app.send_frames(start.elapsed().as_millis() as u32);
        }
        let _ = display.flush_clients();
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
    }
    #[test]
    fn size_arg() { assert_eq!(parse_size("720x1280"), Some((720, 1280))); assert_eq!(parse_size("x"), None); }
}
