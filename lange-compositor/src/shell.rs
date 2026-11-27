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
pub enum Mode { Home, App, Recents }

#[derive(Debug, Clone, PartialEq)]
pub struct AppEntry { pub component: String, pub label: String, pub color: u32 }

impl AppEntry {
    pub fn new(component: &str) -> AppEntry {
        AppEntry { component: component.into(), label: label_for(component), color: color_for(component) }
    }
}

/// O que o compositor deve fazer depois de um toque no shell.
#[derive(Debug, PartialEq)]
pub enum Action { None, Redraw, Launch(String), Key(&'static str) }

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

/// Célula da grade da tela inicial.
const CELL_W: i32 = 128;
const CELL_H: i32 = 124;
const ICON: i32 = 64;

pub struct Shell { pub mode: Mode, pub apps: Vec<AppEntry>, pub recents: Vec<String>, pub font: Font, down: Option<(f64, f64)> }

impl Shell {
    pub fn new() -> Shell { Shell { mode: Mode::Home, apps: vec![], recents: vec![], font: Font::new(), down: None } }

    /// Área do conteúdo (entre as barras).
    fn content(h: usize) -> (i32, i32) { (BAR as i32, h as i32 - NAV as i32) }

    /// Retângulos (x, y, w, h) das células da grade, na ordem de `apps`.
    pub fn grid(&self, w: usize, h: usize) -> Vec<(i32, i32, i32, i32)> {
        let (top, bottom) = Self::content(h);
        let cols = ((w as i32 - 32) / CELL_W).max(1);
        let x0 = (w as i32 - cols * CELL_W) / 2;
        (0..self.apps.len() as i32)
            .map(|i| (x0 + i % cols * CELL_W, top + 24 + i / cols * CELL_H, CELL_W, CELL_H))
            .take_while(|&(_, y, _, ch)| y + ch <= bottom) // ponytail: uma página só; paginação quando tiver muitos apps
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

    /// Toque terminou: vira toque simples se não andou muito. Devolve a ação.
    pub fn touch_up(&mut self, x: f64, y: f64, w: usize, h: usize) -> Action {
        let Some((dx, dy)) = self.down.take() else { return Action::None };
        if (dx - x).abs() > 24.0 || (dy - y).abs() > 24.0 { return Action::None; } // arrasto: ignora (sem gestos ainda)
        self.tap(x as i32, y as i32, w, h)
    }

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
            Mode::Home => self.grid(w, h).into_iter().position(inside).map(|i| self.apps[i].component.clone()),
            Mode::Recents => self.cards(w, h).into_iter().position(inside).map(|i| self.recents[i].clone()),
            Mode::App => None,
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
            Mode::Home => {
                c.gradient(top as usize, bottom as usize, 0xff10_2a43, 0xff0b_3d3a);
                if self.apps.is_empty() {
                    let msg = "Iniciando Android…";
                    let tw = self.font.width(msg, 20);
                    self.font.draw(c, (w as i32 - tw) / 2, (top + bottom) / 2, 20, FG_DIM, msg);
                }
                let cells = self.grid(w, h);
                for (i, (x, y, cw, _)) in cells.into_iter().enumerate() {
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
        // barra de status: nome à esquerda, bateria + relógio à direita
        c.rect(0, 0, w as i32, BAR as i32, BG_BAR);
        self.font.draw(c, 12, 22, 15, FG_DIM, "LangeOS");
        let right = match battery { Some(b) => format!("{b}%  {clock}"), None => clock.to_string() };
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
        let mut s = Shell { mode: Mode::Home, apps: vec![AppEntry::new("a.b/.C"), AppEntry::new("d.e/.F")], recents: vec![], font: Font::new(), down: None };
        let cells = s.grid(w, h);
        let (x, y, cw, ch) = cells[1];
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
        assert_eq!(s.touch_up(300.0, 100.0, w, h), Action::None);
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
        for (m, name) in [(Mode::Home, "home"), (Mode::Recents, "recents")] {
            s.mode = m;
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
        for m in [Mode::Home, Mode::Recents, Mode::App] {
            s.mode = m;
            s.recents = vec!["com.android.settings/.Settings".into()];
            s.draw(&mut Canvas { px: &mut px, w, h }, "12:34", Some(80));
        }
        assert!(px[..w * BAR].iter().any(|&p| p == FG)); // texto da barra de status
        let mut px = vec![0u32; 10 * 10];
        Canvas { px: &mut px, w: 10, h: 10 }.rect(-5, -5, 100, 100, 1); // recorte
        assert!(px.iter().all(|&p| p == 1));
    }
}
