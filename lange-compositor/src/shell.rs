//! Shell nativo do LangeOS: barra de status, barra de navegação, tela inicial (grade de apps) e
//! recentes. Desenho na CPU direto no framebuffer XRGB; lógica de toque pura (testada).
//! Apps vêm do container Android (langedroid `apps`); abrir = `launch`, voltar = `key back`.
use std::collections::HashMap;

/// Altura das barras de status (topo) e de navegação (base), em pixels.
pub const BAR: usize = 32;
pub const NAV: usize = 48;

const BG_BAR: u32 = 0xff1c_2430;
const FG: u32 = 0xffe8_eef4;
const FG_DIM: u32 = 0xff9a_a8b4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode { Locked, Home, App, Recents }

#[derive(Debug, Clone, PartialEq)]
pub struct AppEntry { pub component: String, pub label: String, pub color: u32 }

impl AppEntry {
    pub fn new(component: &str) -> AppEntry {
        AppEntry { component: component.into(), label: label_for(component), color: color_for(component) }
    }
}

/// O que o compositor deve fazer depois de um toque no shell.
#[derive(Debug, PartialEq)]
pub enum Action { None, Redraw, Launch(String), Key(&'static str), Notifs, Volume(u8), SavePin }

/// Nome curto a partir do pacote: com.android.gallery3d -> "Gallery". ponytail: sem o rótulo real do
/// APK (precisaria ler resources.arsc); trocar quando o langedroid devolver o label.
pub fn label_for(component: &str) -> String {
    let pkg = component.split('/').next().unwrap_or(component);
    // apps padrão do AOSP/LineageOS: nome em português
    const KNOWN: [(&str, &str); 18] = [
        ("org.lineageos.jelly", "Navegador"), ("org.lineageos.eleven", "Música"), ("org.lineageos.etar", "Agenda"),
        ("com.android.calendar", "Agenda"), ("com.android.deskclock", "Relógio"), ("com.android.documentsui", "Arquivos"),
        ("com.android.gallery3d", "Galeria"), ("org.lineageos.glimpse", "Galeria"), ("com.android.settings", "Configurações"),
        ("com.android.contacts", "Contatos"), ("com.android.dialer", "Telefone"), ("com.android.messaging", "Mensagens"),
        ("org.lineageos.aperture", "Câmera"), ("com.android.camera2", "Câmera"), ("org.lineageos.recorder", "Gravador"),
        ("com.android.calculator2", "Calculadora"), ("org.lineageos.updater", "Atualizador"), ("com.android.email", "E-mail"),
    ];
    if let Some((_, name)) = KNOWN.iter().find(|(p, _)| *p == pkg) { return name.to_string(); }
    let last = pkg.rsplit('.').next().unwrap_or(pkg);
    let word: String = last.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let word = if word.is_empty() { last.to_string() } else { word };
    let mut c = word.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Cor estável por app (hash simples -> paleta).
fn color_for(component: &str) -> u32 {
    const PALETTE: [u32; 8] = [0xff3b_82f6, 0xffef_4444, 0xff10_b981, 0xfff5_9e0b, 0xff8b_5cf6, 0xffec_4899, 0xff14_b8a6, 0xfff9_7316];
    let h = component.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    PALETTE[h as usize % PALETTE.len()]
}

// ---------- texto ----------

pub struct Font { font: fontdue::Font, cache: HashMap<(char, u32), (fontdue::Metrics, Vec<u8>)> }

impl Font {
    pub fn new() -> Font {
        let font = fontdue::Font::from_bytes(&include_bytes!("../assets/DejaVuSans.ttf")[..], fontdue::FontSettings::default())
            .expect("fonte embutida");
        Font { font, cache: HashMap::new() }
    }
    fn glyph(&mut self, c: char, px: u32) -> &(fontdue::Metrics, Vec<u8>) {
        let f = &self.font;
        self.cache.entry((c, px)).or_insert_with(|| f.rasterize(c, px as f32))
    }
    pub fn width(&mut self, s: &str, px: u32) -> i32 {
        s.chars().map(|c| self.glyph(c, px).0.advance_width.round() as i32).sum()
    }
    /// Texto com a linha de base em `y`.
    pub fn draw(&mut self, fb: &mut Canvas, x: i32, y: i32, px: u32, color: u32, s: &str) {
        let mut pen = x;
        for c in s.chars() {
            let (m, bmp) = self.glyph(c, px).clone();
            let gx = pen + m.xmin;
            let gy = y - m.height as i32 - m.ymin;
            for row in 0..m.height {
                for col in 0..m.width {
                    let a = bmp[row * m.width + col] as u32;
                    if a > 0 { fb.blend_px(gx + col as i32, gy + row as i32, color, a); }
                }
            }
            pen += m.advance_width.round() as i32;
        }
    }
    /// Corta com "…" até caber em `max` pixels.
    pub fn fit(&mut self, s: &str, px: u32, max: i32) -> String {
        if self.width(s, px) <= max { return s.into(); }
        let mut out: String = s.into();
        while !out.is_empty() && self.width(&format!("{out}…"), px) > max { out.pop(); }
        format!("{out}…")
    }
}

// ---------- desenho ----------

pub struct Canvas<'a> { pub px: &'a mut [u32], pub w: usize, pub h: usize }

impl Canvas<'_> {
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (x0, y0) = (x.max(0) as usize, y.max(0) as usize);
        let (x1, y1) = (((x + w).max(0) as usize).min(self.w), ((y + h).max(0) as usize).min(self.h));
        if x0 >= x1 { return; }
        for yy in y0..y1 { self.px[yy * self.w + x0..yy * self.w + x1].fill(color); }
    }
    fn blend_px(&mut self, x: i32, y: i32, color: u32, a: u32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h { return; }
        let d = &mut self.px[y as usize * self.w + x as usize];
        let mix = |s: u32, d: u32| (s * a + d * (255 - a)) / 255;
        let ch = |v: u32, sh: u32| (v >> sh) & 0xff;
        *d = 0xff00_0000 | mix(ch(color, 16), ch(*d, 16)) << 16 | mix(ch(color, 8), ch(*d, 8)) << 8 | mix(ch(color, 0), ch(*d, 0));
    }
    /// Retângulo de cantos arredondados (raio r), borda suavizada.
    pub fn rounded(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, color: u32) {
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                let dx = (x + r - xx).max(xx - (x + w - 1 - r)).max(0) as f32;
                let dy = (y + r - yy).max(yy - (y + h - 1 - r)).max(0) as f32;
                let d = (dx * dx + dy * dy).sqrt() - r as f32;
                let a = ((0.5 - d).clamp(0.0, 1.0) * 255.0) as u32;
                if a > 0 { self.blend_px(xx, yy, color, a); }
            }
        }
    }
    /// Fundo da tela inicial: degradê vertical.
    pub fn gradient(&mut self, y0: usize, y1: usize, top: u32, bottom: u32) {
        let span = (y1 - y0).max(1) as u32;
        for y in y0..y1.min(self.h) {
            let t = (y - y0) as u32;
            let ch = |sh: u32| (((top >> sh) & 0xff) * (span - t) + ((bottom >> sh) & 0xff) * t) / span;
            let c = 0xff00_0000 | ch(16) << 16 | ch(8) << 8 | ch(0);
            self.px[y * self.w..(y + 1) * self.w].fill(c);
        }
    }
}

// ---------- shell ----------

/// Largura dos botões do painel (Bloquear, PIN).
const BTN_W: i32 = 150;

/// Início (x) dos controles deslizantes do painel (texto à esquerda).
const SLIDER_X: i32 = 110;

/// Célula da grade da tela inicial.
const CELL_W: i32 = 128;
const CELL_H: i32 = 124;
const ICON: i32 = 64;

pub struct Shell {
    pub mode: Mode, pub apps: Vec<AppEntry>, pub recents: Vec<String>, pub font: Font, down: Option<(f64, f64)>,
    /// página da grade da tela inicial
    pub page: usize,
    /// painel rápido (puxado da barra de status) aberto por cima de qualquer modo
    pub panel: bool,
    /// notificações do Android: (título, texto)
    pub notifs: Vec<(String, String)>,
    /// brilho da tela em % (10..=100); aplicado pelo compositor
    pub brightness: u8,
    /// volume de mídia do Android (0..=VOL_MAX)
    pub volume: u8,
    /// rede do Android: None = desconhecido, Some(iface vazio) = sem rede
    pub net: Option<String>,
    /// ícones reais renderizados pelo Android (ARGB, ICON x ICON) por componente
    pub icons: std::collections::HashMap<String, Vec<u32>>,
    /// PIN gravado (None = desbloqueio só deslizando)
    pub pin: Option<crate::pin::Pin>,
    /// teclado do PIN aberto (por cima de tudo) e em qual passo
    pub pin_step: Option<PinStep>,
    pin_input: String,
    pin_new: String,
    pin_msg: &'static str,
    pin_fails: u32,
    pin_wait_until: Option<std::time::Instant>,
}

/// Passos do teclado do PIN: desbloquear, ou trocar (atual -> novo -> confirmar).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinStep { Unlock, Current, New, Confirm }

const PIN_MIN: usize = 4;
const PIN_MAX: usize = 8;
/// erros seguidos antes da espera
pub const PIN_TRIES: u32 = 5;
const PIN_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// teclado: 3 colunas x 4 linhas (1-9, ⌫ 0 OK)
const KEY: i32 = 76;
const KEY_GAP: i32 = 96;

pub const VOL_MAX: u8 = 15;

impl Shell {
    pub fn new() -> Shell {
        Shell { mode: Mode::Locked, apps: vec![], recents: vec![], font: Font::new(), down: None, page: 0, panel: false, notifs: vec![], brightness: 100, volume: 8, net: None, icons: Default::default(), pin: None, pin_step: None, pin_input: String::new(), pin_new: String::new(), pin_msg: "", pin_fails: 0, pin_wait_until: None }
    }

    /// Shell recebe o toque (e não a janela Android)?
    pub fn wants_touch(&self) -> bool { self.mode != Mode::App || self.panel || self.pin_step.is_some() }

    /// Células por página e nº de páginas da grade.
    fn per_page(w: usize, h: usize) -> (i32, i32) {
        let (top, bottom) = Self::content(h);
        let cols = ((w as i32 - 32) / CELL_W).max(1);
        let rows = ((bottom - top - 24 - 40) / CELL_H).max(1); // 40 = pontos de página
        (cols, rows)
    }
    pub fn pages(&self, w: usize, h: usize) -> usize {
        let (c, r) = Self::per_page(w, h);
        self.apps.len().div_ceil((c * r) as usize).max(1)
    }

    /// Área do conteúdo (entre as barras).
    fn content(h: usize) -> (i32, i32) { (BAR as i32, h as i32 - NAV as i32) }

    /// Células da página atual: (índice em `apps`, retângulo x, y, w, h).
    pub fn grid(&self, w: usize, h: usize) -> Vec<(usize, (i32, i32, i32, i32))> {
        let (top, _) = Self::content(h);
        let (cols, rows) = Self::per_page(w, h);
        let x0 = (w as i32 - cols * CELL_W) / 2;
        let n = (cols * rows) as usize;
        (self.page * n..((self.page + 1) * n).min(self.apps.len()))
            .map(|i| { let k = (i % n) as i32; (i, (x0 + k % cols * CELL_W, top + 24 + k / cols * CELL_H, CELL_W, CELL_H)) })
            .collect()
    }

    /// Cartões dos recentes (mais novo primeiro).
    fn cards(&self, w: usize, h: usize) -> Vec<(i32, i32, i32, i32)> {
        let (top, bottom) = Self::content(h);
        let (cw, ch) = (220, (bottom - top - 80).clamp(80, 360));
        let n = self.recents.len() as i32;
        let x0 = ((w as i32 - n * (cw + 24) + 24) / 2).max(16);
        (0..n).map(|i| (x0 + i * (cw + 24), top + 40, cw, ch)).collect()
    }

    /// Botões da barra de navegação: voltar, início, recentes (centros x).
    fn nav_buttons(w: usize) -> [i32; 3] { let c = w as i32 / 2; [c - 140, c, c + 140] }

    fn app_by_component(&self, c: &str) -> Option<&AppEntry> { self.apps.iter().find(|a| a.component == c) }

    /// Toque começou no shell (fora da janela Android).
    pub fn touch_down(&mut self, x: f64, y: f64) { self.down = Some((x, y)); }

    /// Toque terminou: gesto (arrasto) ou toque simples. Devolve a ação.
    pub fn touch_up(&mut self, x: f64, y: f64, w: usize, h: usize) -> Action {
        let Some((sx, sy)) = self.down.take() else { return Action::None };
        let (dx, dy) = (x - sx, y - sy);
        let swipe = dx.abs() > 60.0 || dy.abs() > 60.0;
        if self.pin_step.is_some() {
            if swipe { return Action::None; }
            return self.pin_tap(x as i32, y as i32, w, h);
        }
        if self.panel {
            // painel: arrastar p/ cima ou tocar fora fecha
            if (swipe && dy < 0.0) || (!swipe && y as i32 > Self::panel_bottom(h)) { self.panel = false; return Action::Redraw; }
            // controles deslizantes: toque ou arrasto na linha (vale o x final)
            if dy.abs() < 40.0 {
                let f = ((x as i32 - SLIDER_X) as f64 / (w as i32 - 2 * SLIDER_X) as f64).clamp(0.0, 1.0);
                if Self::slider_row(sy as i32) == Some(0) { self.brightness = (10.0 + f * 90.0).round() as u8; return Action::Redraw; }
                if Self::slider_row(sy as i32) == Some(1) { self.volume = (f * VOL_MAX as f64).round() as u8; return Action::Volume(self.volume); }
            }
            if !swipe && (y as i32 - Self::buttons_y()).abs() < 22 {
                if (x as i32) < BTN_W + 24 { self.panel = false; self.mode = Mode::Locked; return Action::Redraw; }
                if (x as i32) < 2 * BTN_W + 40 { self.panel = false; self.open_pin(if self.pin.is_some() { PinStep::Current } else { PinStep::New }); return Action::Redraw; }
            }
            return Action::None;
        }
        if sy < BAR as f64 && dy > 60.0 && self.mode != Mode::Locked { self.panel = true; return Action::Notifs; }
        if self.mode == Mode::Locked {
            if swipe && dy < -80.0 {
                if self.pin.is_some() { self.open_pin(PinStep::Unlock) } else { self.mode = Mode::Home }
                return Action::Redraw;
            }
            return Action::None;
        }
        if self.mode == Mode::Home && swipe && dx.abs() > dy.abs() {
            let last = self.pages(w, h) - 1;
            let page = if dx < 0.0 { (self.page + 1).min(last) } else { self.page.saturating_sub(1) };
            if page == self.page { return Action::None; }
            self.page = page;
            return Action::Redraw;
        }
        if swipe { return Action::None; }
        self.tap(x as i32, y as i32, w, h)
    }

    pub fn pin_fails(&self) -> u32 { self.pin_fails }

    /// Erros seguidos de PIN. A cada PIN_TRIES erros: espera que dobra (30 s, 1 min, 2 min... até 1 h).
    /// Só zera com o PIN certo.
    pub fn set_pin_fails(&mut self, n: u32) {
        self.pin_fails = n;
        // nova espera ao completar cada leva de erros, ou ao carregar a contagem salva (reinício)
        if n >= PIN_TRIES && (n % PIN_TRIES == 0 || self.pin_wait_until.is_none()) {
            let lockouts = n / PIN_TRIES; // 1, 2, 3...
            let wait = PIN_WAIT.saturating_mul(1 << (lockouts - 1).min(7)).min(std::time::Duration::from_secs(3600));
            self.pin_wait_until = Some(std::time::Instant::now() + wait);
        }
    }

    fn buttons_y() -> i32 { Self::slider_y(1) + 52 }

    fn open_pin(&mut self, step: PinStep) {
        self.pin_step = Some(step);
        self.pin_input.clear();
        self.pin_msg = match step { PinStep::Unlock | PinStep::Current => "Digite o PIN", PinStep::New => "Novo PIN (4 a 8 dígitos)", PinStep::Confirm => "Confirme o novo PIN" };
    }

    /// Centro da tecla `i` (0-8 = 1-9, 9 = ⌫, 10 = 0, 11 = OK).
    fn key_center(i: i32, w: usize, h: usize) -> (i32, i32) {
        let (top, bottom) = Self::content(h);
        let y0 = (top + bottom) / 2 - KEY_GAP * 3 / 2 + 50;
        (w as i32 / 2 + (i % 3 - 1) * KEY_GAP, y0 + i / 3 * KEY_GAP)
    }

    fn pin_tap(&mut self, x: i32, y: i32, w: usize, h: usize) -> Action {
        let step = self.pin_step.unwrap();
        // Voltar/Início na barra de navegação cancelam a troca (desbloqueio não tem como pular)
        if y >= h as i32 - NAV as i32 {
            if step != PinStep::Unlock { self.pin_step = None; self.pin_input.clear(); self.pin_new.clear(); return Action::Redraw; }
            return Action::None;
        }
        let Some(k) = (0..12).find(|&i| { let (cx, cy) = Self::key_center(i, w, h); (x - cx).abs() <= KEY / 2 && (y - cy).abs() <= KEY / 2 }) else { return Action::None };
        if self.pin_wait_until.is_some_and(|t| std::time::Instant::now() < t) { self.pin_msg = "Muitas tentativas: aguarde"; return Action::Redraw; }
        match k {
            9 => { self.pin_input.pop(); }
            11 => return self.pin_ok(step),
            _ if self.pin_input.len() < PIN_MAX => self.pin_input.push(char::from(b'0' + if k == 10 { 0 } else { k as u8 + 1 })),
            _ => {}
        }
        Action::Redraw
    }

    fn pin_ok(&mut self, step: PinStep) -> Action {
        let input = std::mem::take(&mut self.pin_input);
        if input.len() < PIN_MIN { self.pin_input = input; self.pin_msg = "Mínimo de 4 dígitos"; return Action::Redraw; }
        match step {
            PinStep::Unlock | PinStep::Current => {
                if self.pin.as_ref().is_some_and(|p| p.check(&input)) {
                    self.pin_fails = 0;
                    if step == PinStep::Unlock { self.pin_step = None; self.mode = Mode::Home } else { self.open_pin(PinStep::New) }
                } else {
                    self.set_pin_fails(self.pin_fails + 1);
                    self.pin_msg = "PIN incorreto";
                }
                Action::Redraw
            }
            PinStep::New => { self.pin_new = input; self.open_pin(PinStep::Confirm); Action::Redraw }
            PinStep::Confirm => {
                let new = std::mem::take(&mut self.pin_new);
                if new != input { self.open_pin(PinStep::New); self.pin_msg = "Não confere: digite de novo"; return Action::Redraw; }
                match crate::pin::Pin::new(&new) {
                    Ok(p) => { self.pin = Some(p); self.pin_step = None; Action::SavePin }
                    Err(_) => { self.pin_step = None; Action::Redraw }
                }
            }
        }
    }

    fn draw_pin(&mut self, c: &mut Canvas) {
        let (w, h) = (c.w, c.h);
        let (top, bottom) = Self::content(h);
        c.gradient(top as usize, bottom as usize, 0xff0b_1a2e, 0xff1d_3b4f);
        let (_, y1) = Self::key_center(0, w, h);
        let mw = self.font.width(self.pin_msg, 20);
        self.font.draw(c, (w as i32 - mw) / 2, y1 - 110, 20, FG, self.pin_msg);
        let n = self.pin_input.len() as i32;
        for i in 0..n.max(PIN_MIN as i32) {
            let x = w as i32 / 2 + (i * 2 - n.max(PIN_MIN as i32) + 1) * 14 - 6;
            c.rounded(x, y1 - 80, 12, 12, 6, if i < n { FG } else { 0xff4a_5866 });
        }
        for i in 0..12 {
            let (cx, cy) = Self::key_center(i, w, h);
            c.rounded(cx - KEY / 2, cy - KEY / 2, KEY, KEY, KEY / 2, 0xff24_3a52);
            let label = match i { 9 => "⌫".to_string(), 10 => "0".into(), 11 => "OK".into(), _ => (i + 1).to_string() };
            let lw = self.font.width(&label, 26);
            self.font.draw(c, cx - lw / 2, cy + 10, 26, FG, &label);
        }
    }

    fn panel_bottom(h: usize) -> i32 { (h as i32 * 3 / 5).max(BAR as i32 + 300) }

    /// Linhas dos controles no painel: 0 = brilho, 1 = volume (y do centro).
    fn slider_y(i: i32) -> i32 { BAR as i32 + 120 + i * 48 }
    fn slider_row(y: i32) -> Option<i32> { (0..2).find(|&i| (y - Self::slider_y(i)).abs() < 22) }

    pub fn tap(&mut self, x: i32, y: i32, w: usize, h: usize) -> Action {
        let inside = |(rx, ry, rw, rh): (i32, i32, i32, i32)| x >= rx && y >= ry && x < rx + rw && y < ry + rh;
        if y >= h as i32 - NAV as i32 {
            let b = Self::nav_buttons(w);
            let hit = b.iter().position(|&cx| (x - cx).abs() < 60);
            return match (hit, self.mode) {
                (Some(0), Mode::App) => Action::Key("back"),
                (Some(0), Mode::Recents) | (Some(1), _) => {
                    let was_app = self.mode == Mode::App;
                    self.mode = Mode::Home;
                    if was_app { Action::Key("home") } else { Action::Redraw }
                }
                (Some(2), _) => { self.mode = if self.mode == Mode::Recents { Mode::Home } else { Mode::Recents }; Action::Redraw }
                _ => Action::None,
            };
        }
        if y < BAR as i32 { return Action::None; }
        let target = match self.mode {
            Mode::Home => self.grid(w, h).into_iter().find(|&(_, r)| inside(r)).map(|(i, _)| self.apps[i].component.clone()),
            Mode::Recents => self.cards(w, h).into_iter().position(inside).map(|i| self.recents[i].clone()),
            Mode::App | Mode::Locked => None,
        };
        match target {
            Some(c) => { self.open(&c); Action::Launch(c) }
            None if self.mode == Mode::Recents => { self.mode = Mode::Home; Action::Redraw }
            None => Action::None,
        }
    }

    /// Marca app como aberto: vai p/ frente dos recentes e o shell passa a mostrar a janela Android.
    pub fn open(&mut self, component: &str) {
        self.recents.retain(|c| c != component);
        self.recents.insert(0, component.into());
        self.recents.truncate(8);
        self.mode = Mode::App;
    }

    /// Ícone: círculo colorido com a inicial.
    fn icon(&mut self, c: &mut Canvas, cx: i32, y: i32, size: i32, app: &AppEntry) {
        if let Some(px) = self.icons.get(&app.component).filter(|p| !p.is_empty()) {
            let (x0, y0) = (cx - ICON / 2, y + (size - ICON) / 2);
            for (i, &p) in px.iter().enumerate() {
                let a = p >> 24;
                if a > 0 { c.blend_px(x0 + i as i32 % ICON, y0 + i as i32 / ICON, p, a); }
            }
            return;
        }
        c.rounded(cx - size / 2, y, size, size, size / 2, app.color);
        let letter: String = app.label.chars().take(1).collect();
        let px = (size * 9 / 20) as u32;
        let lw = self.font.width(&letter, px);
        self.font.draw(c, cx - lw / 2, y + size / 2 + px as i32 * 7 / 20, px, FG, &letter);
    }

    /// Desenha o shell. Em `Mode::App` só as barras (a janela Android já foi desenhada embaixo).
    pub fn draw(&mut self, c: &mut Canvas, clock: &str, battery: Option<u8>) {
        let (w, h) = (c.w, c.h);
        let (top, bottom) = Self::content(h);
        match self.mode {
            Mode::App => {}
            Mode::Locked => {
                c.gradient(top as usize, bottom as usize, 0xff0b_1a2e, 0xff1d_3b4f);
                let cy = (top + bottom) / 2;
                let tw = self.font.width(clock, 110);
                self.font.draw(c, (w as i32 - tw) / 2, cy - 10, 110, FG, clock);
                let d = date_now();
                let dw = self.font.width(&d, 22);
                self.font.draw(c, (w as i32 - dw) / 2, cy + 36, 22, FG_DIM, &d);
                let msg = "Deslize para cima para desbloquear";
                let mw = self.font.width(msg, 16);
                self.font.draw(c, (w as i32 - mw) / 2, bottom - 24, 16, FG_DIM, msg);
            }
            Mode::Home => {
                c.gradient(top as usize, bottom as usize, 0xff10_2a43, 0xff0b_3d3a);
                if self.apps.is_empty() {
                    let msg = "Iniciando Android…";
                    let tw = self.font.width(msg, 20);
                    self.font.draw(c, (w as i32 - tw) / 2, (top + bottom) / 2, 20, FG_DIM, msg);
                }
                let pages = self.pages(w, h);
                if pages > 1 {
                    for p in 0..pages as i32 {
                        let x = w as i32 / 2 + (p * 2 - pages as i32 + 1) * 10 - 4;
                        c.rounded(x, bottom - 24, 8, 8, 4, if p as usize == self.page { FG } else { 0xff4a_5866 });
                    }
                }
                let cells = self.grid(w, h);
                for (i, (x, y, cw, _)) in cells.into_iter() {
                    let app = self.apps[i].clone();
                    self.icon(c, x + cw / 2, y + 8, ICON, &app);
                    let label = self.font.fit(&app.label, 15, cw - 12);
                    let lw = self.font.width(&label, 15);
                    self.font.draw(c, x + (cw - lw) / 2, y + 8 + ICON + 24, 15, FG, &label);
                }
            }
            Mode::Recents => {
                c.gradient(top as usize, bottom as usize, 0xff0d_1117, 0xff16_1b22);
                if self.recents.is_empty() {
                    let msg = "Nenhum app recente";
                    let tw = self.font.width(msg, 20);
                    self.font.draw(c, (w as i32 - tw) / 2, (top + bottom) / 2, 20, FG_DIM, msg);
                }
                for (i, (x, y, cw, ch)) in self.cards(w, h).into_iter().enumerate() {
                    let comp = self.recents[i].clone();
                    let app = self.app_by_component(&comp).cloned().unwrap_or_else(|| AppEntry::new(&comp));
                    c.rounded(x, y, cw, ch, 16, 0xff2a_3340);
                    self.icon(c, x + cw / 2, y + ch / 2 - 48, 72, &app);
                    let label = self.font.fit(&app.label, 17, cw - 16);
                    let lw = self.font.width(&label, 17);
                    self.font.draw(c, x + (cw - lw) / 2, y + ch / 2 + 52, 17, FG, &label);
                }
            }
        }
        if self.panel { self.draw_panel(c, clock, battery); }
        if self.pin_step.is_some() { self.draw_pin(c); }
        // barra de status: nome à esquerda, bateria + relógio à direita
        c.rect(0, 0, w as i32, BAR as i32, BG_BAR);
        self.font.draw(c, 12, 22, 15, FG_DIM, "LangeOS");
        let net = match self.net.as_deref() { Some("") => "Sem rede  ", Some(_) => "Rede  ", None => "" };
        let right = match battery { Some(b) => format!("{net}{b}%  {clock}"), None => format!("{net}{clock}") };
        let rw = self.font.width(&right, 16);
        self.font.draw(c, w as i32 - rw - 12, 22, 16, FG, &right);
        // barra de navegação: ◁ ○ □
        c.rect(0, bottom, w as i32, NAV as i32, BG_BAR);
        let [b, hm, r] = Self::nav_buttons(w);
        let cy = bottom + NAV as i32 / 2;
        for i in 0..14 { c.rect(b - 7 + i, cy - i / 2 - 1, 1, i + 2, FG); } // triângulo apontando p/ esquerda
        c.rounded(hm - 10, cy - 10, 20, 20, 10, FG);
        c.rounded(hm - 7, cy - 7, 14, 14, 7, BG_BAR);
        c.rounded(r - 9, cy - 9, 18, 18, 3, FG);
        c.rounded(r - 6, cy - 6, 12, 12, 2, BG_BAR);
    }
}

impl Shell {
    /// Painel rápido: data/hora, bateria e notificações do Android.
    fn draw_panel(&mut self, c: &mut Canvas, clock: &str, battery: Option<u8>) {
        let w = c.w as i32;
        let bottom = Self::panel_bottom(c.h);
        c.rect(0, BAR as i32, w, bottom - BAR as i32, 0xf015_1b24 | 0xff00_0000);
        c.rect(0, bottom - 2, w, 2, 0xff2f_3a48);
        let mut y = BAR as i32 + 52;
        self.font.draw(c, 24, y, 40, FG, clock);
        let d = date_now();
        let cw = self.font.width(clock, 40);
        self.font.draw(c, 24 + cw + 16, y, 18, FG_DIM, &d);
        if let Some(b) = battery {
            let t = format!("Bateria {b}%");
            let tw = self.font.width(&t, 18);
            self.font.draw(c, w - tw - 24, y, 18, FG_DIM, &t);
        }
        for (i, (name, f)) in [("Brilho", (self.brightness - 10) as f64 / 90.0), ("Volume", self.volume as f64 / VOL_MAX as f64)].into_iter().enumerate() {
            let cy = Self::slider_y(i as i32);
            self.font.draw(c, 24, cy + 6, 15, FG_DIM, name);
            let (x0, tw) = (SLIDER_X, w - 2 * SLIDER_X);
            c.rounded(x0, cy - 4, tw, 8, 4, 0xff2f_3a48);
            let fx = (tw as f64 * f) as i32;
            c.rounded(x0, cy - 4, fx.max(8), 8, 4, 0xff4f_9dde);
            c.rounded(x0 + fx - 10, cy - 10, 20, 20, 10, FG);
        }
        let by = Self::buttons_y();
        let pin_label = if self.pin.is_some() { "Alterar PIN" } else { "Definir PIN" };
        for (i, label) in ["Bloquear", pin_label].into_iter().enumerate() {
            let x = 24 + i as i32 * (BTN_W + 16);
            c.rounded(x, by - 18, BTN_W, 36, 18, 0xff2a_3442);
            let lw = self.font.width(label, 15);
            self.font.draw(c, x + (BTN_W - lw) / 2, by + 6, 15, FG, label);
        }
        y = by + 30;
        if self.notifs.is_empty() {
            self.font.draw(c, 24, y + 24, 17, FG_DIM, "Sem notificações");
        }
        for (title, text) in self.notifs.clone() {
            if y + 64 > bottom - 8 { break; }
            c.rounded(16, y, w - 32, 56, 12, 0xff24_2d3a);
            let t = self.font.fit(&title, 16, w - 64);
            self.font.draw(c, 32, y + 23, 16, FG, &t);
            let tx = self.font.fit(&text, 14, w - 64);
            self.font.draw(c, 32, y + 45, 14, FG_DIM, &tx);
            y += 64;
        }
    }
}

/// "sex, 27 nov 2026" (UTC). ponytail: sem fuso, igual ao relógio.
pub fn date_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_date(secs / 86400)
}

fn format_date(days: u64) -> String {
    // dias desde 1970-01-01 -> data civil (algoritmo de Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    const WD: [&str; 7] = ["qui", "sex", "sáb", "dom", "seg", "ter", "qua"]; // 1970-01-01 = quinta
    const MO: [&str; 12] = ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"];
    format!("{}, {d} {} {y}", WD[(days % 7) as usize], MO[(m - 1) as usize])
}

/// "notifs" -> [(título, texto)]: resposta "ok t1\x1fx1\x1et2\x1fx2".
pub fn parse_notifs(reply: &str) -> Option<Vec<(String, String)>> {
    let body = reply.strip_prefix("ok")?.trim_start();
    Some(body.split('\x1e').filter(|e| !e.is_empty())
        .map(|e| { let (t, x) = e.split_once('\x1f').unwrap_or((e, "")); (t.to_string(), x.to_string()) }).collect())
}

/// Pedido de ícones dos apps que ainda não têm (None se todos têm).
pub fn icons_request(s: &Shell) -> Option<String> {
    let missing: Vec<&str> = s.apps.iter().map(|a| a.component.as_str()).filter(|c| !s.icons.contains_key(*c)).collect();
    (!missing.is_empty()).then(|| format!("icons {ICON} {}", missing.join(" ")))
}

/// "icons" -> [(componente, ARGB)]: resposta "ok comp TAM hex\x1ecomp TAM hex". Só aceita TAM = ICON.
pub fn parse_icons(reply: &str) -> Vec<(String, Vec<u32>)> {
    let Some(body) = reply.strip_prefix("ok") else { return vec![] };
    body.split('\x1e').filter_map(|e| {
        let mut it = e.split_whitespace();
        let (comp, n, hex) = (it.next()?, it.next()?.parse::<i32>().ok()?, it.next()?);
        if n != ICON || hex.len() != (n * n * 8) as usize { return None; }
        let px = (0..hex.len()).step_by(8).map(|i| u32::from_str_radix(&hex[i..i + 8], 16)).collect::<Result<_, _>>().ok()?;
        Some((comp.to_string(), px))
    }).collect()
}

/// Nível da bateria (%) em /sys/class/power_supply/*/capacity; None sem bateria (QEMU, desktop).
pub fn battery() -> Option<u8> {
    std::fs::read_dir("/sys/class/power_supply").ok()?.flatten()
        .find_map(|e| std::fs::read_to_string(e.path().join("capacity")).ok()?.trim().parse().ok())
}

/// "apps" -> lista de componentes (resposta "ok comp1 comp2 ...").
pub fn parse_apps(reply: &str) -> Option<Vec<AppEntry>> {
    let mut apps: Vec<AppEntry> = reply.strip_prefix("ok")?.split_whitespace().map(AppEntry::new).collect();
    apps.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    Some(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(label_for("com.android.gallery3d/.app.GalleryActivity"), "Galeria");
        assert_eq!(label_for("com.example.photos3d/.Main"), "Photos");
        assert_eq!(label_for("org.lineageos.jelly/.MainActivity"), "Navegador");
        assert_eq!(label_for("com.android.settings/.Settings"), "Configurações");
    }

    #[test]
    fn apps_reply() {
        let a = parse_apps("ok org.lineageos.jelly/.Main com.android.calendar/.Cal").unwrap();
        assert_eq!(a.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Agenda", "Navegador"]);
        assert!(parse_apps("erro x").is_none());
    }

    #[test]
    fn taps() {
        let (w, h) = (1280, 800);
        let mut s = Shell::new();
        s.mode = Mode::Home;
        s.apps = vec![AppEntry::new("a.b/.C"), AppEntry::new("d.e/.F")];
        let cells = s.grid(w, h);
        let (_, (x, y, cw, ch)) = cells[1];
        assert_eq!(s.tap(x + cw / 2, y + ch / 2, w, h), Action::Launch("d.e/.F".into()));
        assert_eq!(s.mode, Mode::App);
        assert_eq!(s.recents, ["d.e/.F"]);
        let nav_y = h as i32 - 10;
        let [back, home, rec] = Shell::nav_buttons(w);
        assert_eq!(s.tap(back, nav_y, w, h), Action::Key("back"));
        assert_eq!(s.tap(home, nav_y, w, h), Action::Key("home"));
        assert_eq!(s.mode, Mode::Home);
        assert_eq!(s.tap(rec, nav_y, w, h), Action::Redraw);
        assert_eq!(s.mode, Mode::Recents);
        let (x, y, cw, ch) = s.cards(w, h)[0];
        assert_eq!(s.tap(x + cw / 2, y + ch / 2, w, h), Action::Launch("d.e/.F".into()));
        // arrasto não é toque
        s.mode = Mode::Home;
        s.touch_down(10.0, 100.0);
        assert_eq!(s.touch_up(300.0, 100.0, w, h), Action::None); // 1 página: arrasto não muda nada
        // páginas
        s.apps = (0..60).map(|i| AppEntry::new(&format!("p{i}.x/.M"))).collect();
        assert!(s.pages(w, h) >= 2);
        s.touch_down(900.0, 300.0);
        assert_eq!(s.touch_up(600.0, 310.0, w, h), Action::Redraw);
        assert_eq!(s.page, 1);
        assert!(s.grid(w, h)[0].0 > 0);
        // painel: puxar da barra de status abre; arrastar p/ cima fecha
        s.touch_down(600.0, 10.0);
        assert_eq!(s.touch_up(600.0, 300.0, w, h), Action::Notifs);
        assert!(s.panel && s.wants_touch());
        // controles: volume no fim da linha = máximo; brilho no começo = mínimo
        let (vy, by) = (Shell::slider_y(1) as f64, Shell::slider_y(0) as f64);
        s.touch_down(640.0, vy);
        assert_eq!(s.touch_up(1279.0, vy, w, h), Action::Volume(VOL_MAX));
        s.touch_down(0.0, by);
        assert_eq!(s.touch_up(0.0, by, w, h), Action::Redraw);
        assert_eq!(s.brightness, 10);
        assert!(s.panel);
        s.touch_down(600.0, 300.0);
        assert_eq!(s.touch_up(600.0, 100.0, w, h), Action::Redraw);
        assert!(!s.panel);
        // bloqueio: só deslizar p/ cima desbloqueia
        s.mode = Mode::Locked;
        s.touch_down(600.0, 500.0);
        assert_eq!(s.touch_up(600.0, 495.0, w, h), Action::None);
        s.touch_down(600.0, 600.0);
        assert_eq!(s.touch_up(600.0, 300.0, w, h), Action::Redraw);
        assert_eq!(s.mode, Mode::Home);
    }

    #[test]
    fn icons() {
        let hex = "ff00ff00".repeat((ICON * ICON) as usize);
        let got = parse_icons(&format!("ok a.b/.C {ICON} {hex}\x1ebad 2 00"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "a.b/.C");
        assert_eq!(got[0].1[0], 0xff00ff00);
        let mut s = Shell::new();
        s.apps = vec![AppEntry::new("a.b/.C"), AppEntry::new("d.e/.F")];
        s.icons.extend(got);
        assert_eq!(icons_request(&s).unwrap(), format!("icons {ICON} d.e/.F"));
    }

    #[test]
    fn pin_flow() {
        let (w, h) = (1280, 800);
        let mut s = Shell::new();
        let key = |s: &mut Shell, i: i32| { let (x, y) = Shell::key_center(i, w, h); s.touch_down(x as f64, y as f64); s.touch_up(x as f64, y as f64, w, h) };
        let type_pin = |s: &mut Shell, digits: &str| -> Action {
            for d in digits.bytes() { let i = if d == b'0' { 10 } else { (d - b'1') as i32 }; key(s, i); }
            key(s, 11)
        };
        // sem PIN: painel -> Definir PIN -> novo + confirmação
        s.mode = Mode::Home;
        s.panel = true;
        let by = Shell::buttons_y() as f64;
        s.touch_down(24.0 + BTN_W as f64 + 30.0, by);
        assert_eq!(s.touch_up(24.0 + BTN_W as f64 + 30.0, by, w, h), Action::Redraw);
        assert_eq!(s.pin_step, Some(PinStep::New));
        assert_eq!(type_pin(&mut s, "123"), Action::Redraw); // curto demais
        assert_eq!(s.pin_step, Some(PinStep::New));
        s.pin_input.clear();
        type_pin(&mut s, "2580");
        assert_eq!(type_pin(&mut s, "2580"), Action::SavePin);
        assert!(s.pin.is_some() && s.pin_step.is_none());
        // bloqueio: deslizar abre o teclado; PIN errado não entra, certo entra
        s.mode = Mode::Locked;
        s.touch_down(600.0, 600.0);
        s.touch_up(600.0, 300.0, w, h);
        assert_eq!(s.pin_step, Some(PinStep::Unlock));
        assert!(s.wants_touch());
        type_pin(&mut s, "1111");
        assert_eq!(s.mode, Mode::Locked);
        type_pin(&mut s, "2580");
        assert_eq!(s.mode, Mode::Home);
        assert!(s.pin_step.is_none());
        // 5 erros: espera, mesmo com o PIN certo; a espera dobra
        s.mode = Mode::Locked;
        s.open_pin(PinStep::Unlock);
        for _ in 0..PIN_TRIES { type_pin(&mut s, "0000"); }
        type_pin(&mut s, "2580");
        assert_eq!(s.mode, Mode::Locked);
        let left = |s: &Shell| s.pin_wait_until.unwrap().saturating_duration_since(std::time::Instant::now());
        assert!(left(&s) > std::time::Duration::from_secs(25));
        // reinício com 10 erros salvos: espera de 1 min já ativa
        let mut r = Shell::new();
        r.set_pin_fails(2 * PIN_TRIES);
        assert!(left(&r) > std::time::Duration::from_secs(55));
    }

    #[test]
    fn dates_and_notifs() {
        assert_eq!(format_date(0), "qui, 1 jan 1970");
        assert_eq!(format_date(20_784), "sex, 27 nov 2026");
        assert_eq!(parse_notifs("ok A\x1fb\x1eC\x1f").unwrap(), [("A".into(), "b".into()), ("C".into(), String::new())]);
        assert_eq!(parse_notifs("ok").unwrap(), Vec::<(String, String)>::new());
    }

    /// Preview: `SHELL_PREVIEW=/tmp/x cargo test -- --ignored preview` grava x-home.ppm e x-recents.ppm.
    #[test]
    #[ignore]
    fn preview() {
        let out = std::env::var("SHELL_PREVIEW").expect("SHELL_PREVIEW=prefixo");
        let (w, h) = (1280, 800);
        let mut s = Shell::new();
        s.apps = parse_apps("ok com.android.settings/.Settings com.android.gallery3d/.app.GalleryActivity com.android.calendar/.AllInOneActivity org.lineageos.jelly/.MainActivity com.android.contacts/.Main com.android.deskclock/.DeskClock org.lineageos.eleven/.Main com.android.documentsui/.Launcher org.lineageos.etar/.Main com.android.camera2/.Camera").unwrap();
        s.recents = vec!["com.android.gallery3d/.app.GalleryActivity".into(), "com.android.settings/.Settings".into()];
        s.notifs = vec![("Atualização do sistema".into(), "LangeOS 0.1 pronto para instalar".into()), ("Agenda".into(), "Reunião às 15:00".into())];
        for (m, panel, name) in [(Mode::Home, false, "home"), (Mode::Recents, false, "recents"), (Mode::Locked, false, "locked"), (Mode::Home, true, "panel"), (Mode::Locked, false, "pin")] {
            s.mode = m;
            s.panel = panel;
            if name == "pin" { s.open_pin(PinStep::Unlock); s.pin_input = "25".into(); } else { s.pin_step = None; }
            let mut px = vec![0u32; w * h];
            s.draw(&mut Canvas { px: &mut px, w, h }, "10:42", Some(76));
            let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
            for p in px { ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]); }
            std::fs::write(format!("{out}-{name}.ppm"), ppm).unwrap();
        }
    }

    #[test]
    fn draws_all_modes() {
        let (w, h) = (640, 480);
        let mut px = vec![0u32; w * h];
        let mut s = Shell::new();
        s.apps = parse_apps("ok com.android.settings/.Settings").unwrap();
        s.notifs = vec![("Título".into(), "texto".into())];
        for (m, panel) in [(Mode::Locked, false), (Mode::Home, false), (Mode::Recents, false), (Mode::App, true)] {
            s.mode = m;
            s.panel = panel;
            s.recents = vec!["com.android.settings/.Settings".into()];
            s.draw(&mut Canvas { px: &mut px, w, h }, "12:34", Some(80));
        }
        assert!(px[..w * BAR].iter().any(|&p| p == FG)); // texto da barra de status
        let mut px = vec![0u32; 10 * 10];
        Canvas { px: &mut px, w: 10, h: 10 }.rect(-5, -5, 100, 100, 1); // recorte
        assert!(px.iter().all(|&p| p == 1));
    }
}
