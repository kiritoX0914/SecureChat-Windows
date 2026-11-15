//! egui UI. All protocol work happens in `securechat_core` on its own threads;
//! the UI renders under the core lock, collects `Action`s and applies them
//! after releasing it (file dialogs never block the engine).
use egui::{Align, Color32, CornerRadius, Key, Layout, Modifiers, RichText, Sense, Stroke, TextureHandle, Vec2};
use securechat_core::chat::{Config, Core, CoreHandle};
use securechat_core::model::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(0x2A, 0x8B, 0xF2);
const MAX_RENDERED: usize = 300;

pub enum Action {
    Send(String),
    PickFiles { images_only: bool },
    PasteClipboard { from_menu: bool },
    SendPastedImage,
    DroppedFiles(Vec<PathBuf>),
    Reply(String),
    CopyText(String),
    Delete { ids: Vec<String>, everyone: bool },
    Resend(String),
    Open(PathBuf),
    SaveAs(PathBuf, String),
    MarkRead(String),
    Scan,
    ConnectIp(String, u16),
    CreateGroup(String, Vec<String>),
    RenameGroup(String, String),
    AddMembers(String, Vec<String>),
    RemoveMember(String, String),
    LeaveGroup(String),
    ClearChat(String, bool),
    DeleteChat(String),
    SetName(String),
    PickAvatar,
    RemoveAvatar,
    Typing(String, bool),
    ClearError,
}

#[derive(Default)]
struct Dialogs {
    connect: bool,
    connect_host: String,
    connect_port: String,
    new_group: bool,
    group_name: String,
    group_pick: HashSet<String>,
    group_info: bool,
    rename_buf: String,
    add_pick: HashSet<String>,
    settings: bool,
    name_buf: String,
    confirm_delete_chat: Option<String>,
    confirm_clear: Option<String>,
    clear_everyone: bool,
    confirm_delete_selected: bool,
}

pub struct App {
    h: CoreHandle,
    selected: Option<String>,
    draft: HashMap<String, String>,
    reply_to: Option<String>,
    select_mode: bool,
    selection: HashSet<String>,
    textures: HashMap<String, Option<TextureHandle>>,
    dlg: Dialogs,
    ime_composing: bool,
    v_was_down: bool,
    last_typing_sent: Option<Instant>,
    typing_active: bool,
    scroll_to: Option<String>,
    scroll_bottom: bool,
    show_all: HashSet<String>,
    toast: Option<(String, Instant)>,
    paste_preview: Option<(securechat_core::media::PreparedImage, TextureHandle)>,
    ime_commit_frame: bool,
    last_draft_change: Option<Instant>,
    focus_composer: bool,
    bubble_w: HashMap<String, f32>,
    dlg_sig: String,
}

fn load_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let candidates: Vec<(PathBuf, u32)> = vec![
        (PathBuf::from(&windir).join("Fonts").join("msyh.ttc"), 0),
        (PathBuf::from(&windir).join("Fonts").join("msyh.ttf"), 0),
        (PathBuf::from(&windir).join("Fonts").join("Deng.ttf"), 0),
        (PathBuf::from(&windir).join("Fonts").join("simhei.ttf"), 0),
        (PathBuf::from(&windir).join("Fonts").join("simsun.ttc"), 0),
        (PathBuf::from("/System/Library/Fonts/PingFang.ttc"), 0),
        (PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"), 2),
        (PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"), 2),
    ];
    let mut added = vec![];
    for (p, idx) in candidates {
        if added.len() >= 1 { break; }
        if let Ok(bytes) = std::fs::read(&p) {
            let mut fd = egui::FontData::from_owned(bytes);
            fd.index = idx;
            let name = format!("cjk-{}", added.len());
            fonts.font_data.insert(name.clone(), std::sync::Arc::new(fd));
            added.push(name);
        }
    }
    // Segoe UI Symbol / Emoji for ✓ 🕓 etc. (monochrome outlines)
    for f in ["seguisym.ttf", "seguiemj.ttf"] {
        if let Ok(bytes) = std::fs::read(PathBuf::from(&windir).join("Fonts").join(f)) {
            fonts.font_data.insert(f.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            added.push(f.into());
        }
    }
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(fam.clone()).or_default();
        // CJK font right after the default UI font so Latin keeps egui's look.
        for (i, n) in added.iter().enumerate() { list.insert((1 + i).min(list.len()), n.clone()); }
    }
    ctx.set_fonts(fonts);
}

fn style(ctx: &egui::Context) {
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.interaction.selectable_labels = true;
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.5));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
    });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        load_system_fonts(&cc.egui_ctx);
        style(&cc.egui_ctx);
        let h = Core::start(Config { port: 47823, dial_known_on_start: true, enable_beacon: true, ..Default::default() });
        let ctx = cc.egui_ctx.clone();
        h.set_notify(Box::new(move || ctx.request_repaint()));
        App {
            h,
            selected: None,
            draft: HashMap::new(),
            reply_to: None,
            select_mode: false,
            selection: HashSet::new(),
            textures: HashMap::new(),
            dlg: Dialogs { connect_port: "47823".into(), ..Default::default() },
            ime_composing: false,
            v_was_down: false,
            last_typing_sent: None,
            typing_active: false,
            scroll_to: None,
            scroll_bottom: true,
            show_all: HashSet::new(),
            toast: None,
            paste_preview: None,
            ime_commit_frame: false,
            last_draft_change: None,
            focus_composer: true,
            bubble_w: HashMap::new(),
            dlg_sig: String::new(),
        }
    }
}

// ---------- helpers ----------

fn fmt_time(ts: f64) -> String {
    use chrono::{Local, TimeZone};
    let unix = ts + REFERENCE_EPOCH_OFFSET;
    let Some(dt) = Local.timestamp_opt(unix as i64, 0).single() else { return String::new() };
    let now = Local::now();
    if dt.date_naive() == now.date_naive() { dt.format("%H:%M").to_string() } else { dt.format("%m-%d %H:%M").to_string() }
}

fn name_color(id: &str) -> Color32 {
    let palette = [
        Color32::from_rgb(0xE5, 0x73, 0x73), Color32::from_rgb(0x64, 0xB5, 0xF6), Color32::from_rgb(0x81, 0xC7, 0x84),
        Color32::from_rgb(0xFF, 0xB7, 0x4D), Color32::from_rgb(0xBA, 0x68, 0xC8), Color32::from_rgb(0x4D, 0xB6, 0xAC),
        Color32::from_rgb(0xF0, 0x62, 0x92), Color32::from_rgb(0x90, 0xA4, 0xAE),
    ];
    let h = id.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    palette[(h % palette.len() as u32) as usize]
}

fn initials(name: &str) -> String {
    name.trim().chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
}

fn hash_bytes(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

fn texture_from_bytes(ctx: &egui::Context, key: &str, bytes: &[u8], max: u32) -> Option<TextureHandle> {
    let img = securechat_core::media::decode(bytes).ok()?;
    let img = securechat_core::media::resize_max(&img, max);
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture(key, ci, egui::TextureOptions::LINEAR))
}

#[cfg(windows)]
fn v_key_down() -> bool {
    unsafe { (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x56) as u16 & 0x8000) != 0 }
}
#[cfg(not(windows))]
fn v_key_down() -> bool { false }

impl App {
    fn texture(&mut self, ctx: &egui::Context, key: &str, load: impl FnOnce() -> Option<Vec<u8>>, max: u32) -> Option<TextureHandle> {
        if let Some(t) = self.textures.get(key) { return t.clone(); }
        let t = load().and_then(|b| texture_from_bytes(ctx, key, &b, max));
        self.textures.insert(key.to_string(), t.clone());
        t
    }

    fn avatar(&mut self, ui: &mut egui::Ui, id: &str, name: &str, data: Option<&Vec<u8>>, size: f32, online: Option<bool>, group: bool) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
        let tex = data.and_then(|d| {
            let key = format!("av-{:x}", hash_bytes(d));
            let bytes = d.clone();
            self.texture(ui.ctx(), &key, move || Some(bytes), 128)
        });
        let p = ui.painter();
        match tex {
            Some(t) => {
                egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), Vec2::splat(size)))
                    .corner_radius(CornerRadius::same((size / 2.0) as u8))
                    .paint_at(ui, rect);
            }
            None => {
                p.circle_filled(rect.center(), size / 2.0, name_color(id));
                let label = if group { "👥".to_string() } else { initials(name) };
                p.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(size * 0.42), Color32::WHITE);
            }
        }
        if let Some(on) = online {
            if on {
                let c = rect.right_bottom() - Vec2::splat(size * 0.14);
                ui.painter().circle(c, size * 0.13, Color32::from_rgb(0x34, 0xC7, 0x59), Stroke::new(2.0_f32, ui.visuals().panel_fill));
            }
        }
        resp
    }

    // ---------------- chat list ----------------

    fn chat_list(&mut self, ui: &mut egui::Ui, core: &Core, actions: &mut Vec<Action>) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let me = core.local_device_id.clone();
            let r = self.avatar(ui, &me, &core.display_name, core.avatar_data.as_ref(), 36.0, None, false);
            if r.on_hover_text("Profile & settings").clicked() { self.open_settings(core); }
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(&core.display_name).strong().size(16.0)).selectable(false).truncate());
                ui.add(egui::Label::new(RichText::new("SecureChat · end-to-end encrypted").small().weak()).selectable(false));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙").on_hover_text("Settings").clicked() { self.open_settings(core); }
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let scan = ui.add_enabled(!core.is_scanning, egui::Button::new(if core.is_scanning { "⟳ Scanning…" } else { "⟳ Scan" }));
            if scan.on_hover_text("Search the local network (TCP 47823) once").clicked() { actions.push(Action::Scan); }
            if ui.button("🔌 Connect by IP").clicked() { self.dlg.connect = true; }
            if ui.button("👥 New group").clicked() {
                self.dlg.new_group = true;
                self.dlg.group_name.clear();
                self.dlg.group_pick.clear();
            }
        });
        ui.add(egui::Label::new(RichText::new(&core.scan_status).small().weak()).selectable(false));
        ui.separator();

        let ids = core.sorted_conversation_ids();
        if ids.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No chats yet").strong());
                ui.label(RichText::new("Open SecureChat on your phone or Mac on the same network, then press Scan or use Connect by IP.").weak());
                if let Some(p) = core.listening_port { ui.label(RichText::new(format!("This PC listens on port {p}")).small().weak()); }
            });
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for id in ids {
                let Some(c) = core.conversations.get(&id) else { continue };
                self.chat_row(ui, core, c, actions);
            }
        });
    }

    fn chat_row(&mut self, ui: &mut egui::Ui, core: &Core, c: &Conversation, actions: &mut Vec<Action>) {
        let selected = self.selected.as_deref() == Some(c.id.as_str());
        let online = core.is_online(c);
        let fill = if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.55) } else { Color32::TRANSPARENT };
        let frame = egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(6, 6));
        let inner = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let av = if c.is_group() { c.group.as_ref().and_then(|g| g.avatar_data.as_ref()) } else { c.peer.avatar_data.as_ref() };
                self.avatar(ui, &c.id, &c.peer.display_name, av, 44.0, Some(online), c.is_group());
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let t = c.last_message().map(|m| fmt_time(m.timestamp)).unwrap_or_default();
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(t).small().weak()).selectable(false));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(&c.peer.display_name).strong()).selectable(false).truncate());
                            });
                        });
                    });
                    ui.horizontal(|ui| {
                        let preview = if c.is_typing { "typing…".to_string() } else {
                            c.last_message().map(|m| {
                                let p = m.preview_text().replace('\n', " ");
                                if c.is_group() && m.sender_id != core.local_device_id { format!("{}: {}", core.sender_name(m, &c.id), p) }
                                else if m.sender_id == core.local_device_id { format!("You: {p}") } else { p }
                            }).unwrap_or_else(|| if online { "online".into() } else { "offline".into() })
                        };
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if c.unread_count > 0 {
                                let txt = RichText::new(c.unread_count.to_string()).small().color(Color32::WHITE);
                                egui::Frame::new().fill(ACCENT).corner_radius(CornerRadius::same(9)).inner_margin(egui::Margin::symmetric(5, 1)).show(ui, |ui| { ui.add(egui::Label::new(txt).selectable(false)); });
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(preview).weak()).selectable(false).truncate());
                            });
                        });
                    });
                });
            });
        });
        let resp = ui.interact(inner.response.rect, ui.id().with(("row", &c.id)), Sense::click());
        if resp.clicked() {
            if self.selected.as_deref() != Some(c.id.as_str()) {
                self.selected = Some(c.id.clone());
                self.reply_to = None;
                self.select_mode = false;
                self.selection.clear();
                self.scroll_bottom = true;
            }
        }
        let cid = c.id.clone();
        let is_group = c.is_group();
        resp.context_menu(|ui| {
            if ui.button("Clear chat…").clicked() { self.dlg.confirm_clear = Some(cid.clone()); self.dlg.clear_everyone = false; ui.close_menu(); }
            let label = if is_group { "Leave & delete group…" } else { "Delete chat…" };
            if ui.button(RichText::new(label).color(Color32::from_rgb(0xE0, 0x4F, 0x4F))).clicked() { self.dlg.confirm_delete_chat = Some(cid.clone()); ui.close_menu(); }
        });
        let _ = actions;
    }

    fn open_settings(&mut self, core: &Core) {
        self.dlg.settings = true;
        self.dlg.name_buf = core.display_name.clone();
    }
}

impl App {
    fn conversation(&mut self, ui: &mut egui::Ui, core: &Core, cid: &str, actions: &mut Vec<Action>) {
        let Some(c) = core.conversations.get(cid) else { self.selected = None; return };
        let online = core.is_online(c);

        // Header
        egui::TopBottomPanel::top("chat_header").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let av = if c.is_group() { c.group.as_ref().and_then(|g| g.avatar_data.as_ref()) } else { c.peer.avatar_data.as_ref() };
                self.avatar(ui, &c.id, &c.peer.display_name, av, 38.0, Some(online), c.is_group());
                ui.vertical(|ui| {
                    ui.label(RichText::new(&c.peer.display_name).strong().size(16.0));
                    let sub = if let Some(g) = &c.group {
                        let on = g.members.iter().filter(|m| **m != core.local_device_id && core.is_connected(m)).count();
                        if !g.members.contains(&core.local_device_id) { "you left this group".to_string() }
                        else { format!("{} members, {} online", g.members.len(), on) }
                    } else if c.is_typing { "typing…".into() }
                    else if online { "online".into() }
                    else { format!("offline · last seen {}", fmt_time(c.peer.last_seen)) };
                    let r = ui.label(RichText::new(sub).small().color(if online { ACCENT } else { ui.visuals().weak_text_color() }));
                    if !c.is_group() {
                        r.on_hover_text(format!("Safety fingerprint: {}\nLast address: {}", c.peer.fingerprint(), c.peer.last_address.clone().unwrap_or_else(|| "-".into())));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.menu_button("⋯", |ui| {
                        if c.is_group() && ui.button("Group info…").clicked() {
                            self.dlg.group_info = true;
                            self.dlg.rename_buf = c.peer.display_name.clone();
                            self.dlg.add_pick.clear();
                            ui.close_menu();
                        }
                        if ui.button("Select messages").clicked() { self.select_mode = true; self.selection.clear(); ui.close_menu(); }
                        if ui.button("Clear chat…").clicked() { self.dlg.confirm_clear = Some(c.id.clone()); self.dlg.clear_everyone = false; ui.close_menu(); }
                        let label = if c.is_group() { "Leave & delete group…" } else { "Delete chat…" };
                        if ui.button(label).clicked() { self.dlg.confirm_delete_chat = Some(c.id.clone()); ui.close_menu(); }
                    });
                    if c.is_group() && ui.button("👥 Info").clicked() {
                        self.dlg.group_info = true;
                        self.dlg.rename_buf = c.peer.display_name.clone();
                        self.dlg.add_pick.clear();
                    }
                    if !self.select_mode && ui.button("☑ Select").clicked() { self.select_mode = true; self.selection.clear(); }
                });
            });
            ui.add_space(4.0);
        });

        // Composer / selection bar
        egui::TopBottomPanel::bottom("composer").show_inside(ui, |ui| {
            ui.add_space(6.0);
            if self.select_mode {
                ui.horizontal(|ui| {
                    ui.label(format!("{} selected", self.selection.len()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() { self.select_mode = false; self.selection.clear(); }
                        let any = !self.selection.is_empty();
                        if ui.add_enabled(any, egui::Button::new(RichText::new("🗑 Delete…").color(Color32::from_rgb(0xE0, 0x4F, 0x4F)))).clicked() {
                            self.dlg.confirm_delete_selected = true;
                        }
                        if ui.button("Select all").clicked() { self.selection = c.messages.iter().map(|m| m.id.clone()).collect(); }
                    });
                });
            } else {
                self.composer(ui, core, c, actions);
            }
            ui.add_space(6.0);
        });

        // Messages
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let total = c.messages.len();
            let show_all = self.show_all.contains(cid);
            let start = if show_all || total <= MAX_RENDERED { 0 } else { total - MAX_RENDERED };
            let sa = egui::ScrollArea::vertical().id_salt(("msgs", cid)).auto_shrink([false, false]).stick_to_bottom(true);
            sa.show(ui, |ui| {
                ui.add_space(6.0);
                if start > 0 {
                    ui.vertical_centered(|ui| { if ui.button(format!("Show {} earlier messages", start)).clicked() { self.show_all.insert(cid.to_string()); } });
                }
                if total == 0 {
                    ui.vertical_centered(|ui| { ui.add_space(30.0); ui.label(RichText::new("No messages here yet. Say hi 👋").weak()); });
                }
                for m in &c.messages[start..] {
                    self.message(ui, core, c, m, actions);
                }
                ui.add_space(8.0);
                if self.scroll_bottom {
                    ui.scroll_to_cursor(Some(Align::BOTTOM));
                    self.scroll_bottom = false;
                }
            });
        });
    }

    fn message(&mut self, ui: &mut egui::Ui, core: &Core, c: &Conversation, m: &ChatMessage, actions: &mut Vec<Action>) {
        let mine = m.sender_id == core.local_device_id;
        let dark = ui.visuals().dark_mode;
        let (bg, fg) = if mine { (ACCENT, Color32::WHITE) } else if dark { (Color32::from_gray(52), Color32::from_gray(235)) } else { (Color32::WHITE, Color32::from_gray(20)) };
        let layout = Layout::left_to_right(Align::TOP);
        let full_w = ui.available_width();
        let mut text_resp: Option<egui::Response> = None;
        let row = ui.allocate_ui_with_layout(Vec2::new(full_w, 0.0), layout, |ui| {
            if self.select_mode {
                let mut on = self.selection.contains(&m.id);
                if ui.checkbox(&mut on, "").changed() {
                    if on { self.selection.insert(m.id.clone()); } else { self.selection.remove(&m.id); }
                }
            }
            if c.is_group() && !mine {
                self.avatar(ui, &m.sender_id, &core.sender_name(m, &c.id), core.avatar_for(&m.sender_id), 30.0, None, false);
            }
            let max_w = (full_w * 0.72).max(220.0).min(ui.available_width());
            if mine {
                // Right-align using last frame's bubble width (egui lays out in one pass).
                let w = match self.bubble_w.get(&m.id) { Some(w) => *w, None => { ui.ctx().request_repaint(); max_w } };
                ui.add_space((ui.available_width() - w - 2.0).max(0.0));
            }
            let stroke = if !mine && !dark { Stroke::new(1.0_f32, Color32::from_gray(222)) } else { Stroke::NONE };
            let bubble = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| egui::Frame::new().fill(bg).stroke(stroke).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::symmetric(10, 7)).show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.vertical(|ui| {
                    ui.style_mut().visuals.override_text_color = Some(fg);
                    if c.is_group() && !mine {
                        ui.label(RichText::new(core.sender_name(m, &c.id)).small().strong().color(if mine { fg } else { name_color(&m.sender_id) }));
                    }
                    if let Some(rid) = &m.reply_to_id {
                        let qbg = if mine { Color32::from_black_alpha(56) /* darker inset: white text stays readable on blue */ } else if dark { Color32::from_white_alpha(18) } else { Color32::from_black_alpha(14) };
                        let q = egui::Frame::new().fill(qbg).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 4 }).show(ui, |ui| {
                            ui.set_max_width(max_w - 20.0);
                            ui.vertical(|ui| {
                                let who = m.reply_to_sender.clone().unwrap_or_default();
                                ui.add(egui::Label::new(RichText::new(who).small().strong().color(if mine { Color32::WHITE } else { ACCENT })).selectable(false));
                                let t = m.reply_to_text.clone().unwrap_or_default().replace('\n', " ");
                                let qfg = if mine { Color32::from_white_alpha(230) } else { ui.visuals().weak_text_color() };
                                ui.add(egui::Label::new(RichText::new(t).small().color(qfg)).selectable(false).truncate());
                            });
                        });
                        let bar = egui::Rect::from_min_size(q.response.rect.left_top(), Vec2::new(3.0, q.response.rect.height()));
                        ui.painter().rect_filled(bar, CornerRadius::same(2), if mine { Color32::WHITE } else { ACCENT });
                        let qr = ui.interact(q.response.rect, ui.id().with(("q", &m.id)), Sense::click());
                        if qr.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() { self.scroll_to = Some(rid.clone()); }
                    }
                    text_resp = self.message_body(ui, core, m, mine, fg, actions);
                    // footer
                    ui.horizontal(|ui| {
                        let mut t = fmt_time(m.timestamp);
                        if mine {
                            let s = match m.status {
                                STATUS_SENDING => "🕓".to_string(),
                                STATUS_SENT => "✓".into(),
                                STATUS_DELIVERED => "✓✓".into(),
                                STATUS_READ => "✓✓ read".into(),
                                _ => "❗ not sent".into(),
                            };
                            t = format!("{t}  {s}");
                        }
                        let color = if mine { Color32::from_white_alpha(200) } else { ui.visuals().weak_text_color() };
                        ui.add(egui::Label::new(RichText::new(t).small().color(color)).selectable(false));
                        if mine && m.status == STATUS_SENDING && core.has_pending_for(m) {
                            ui.add(egui::Label::new(RichText::new("queued").small().color(color)).selectable(false))
                                .on_hover_text("Recipient offline – will be sent automatically when they reconnect");
                        }
                    });
                });
            }));
            bubble.response
        });
        let bubble_rect = row.inner.rect;
        if mine {
            let prev = self.bubble_w.insert(m.id.clone(), bubble_rect.width());
            if prev.map_or(true, |p| (p - bubble_rect.width()).abs() > 0.5) { ui.ctx().request_repaint(); }
        }
        let dbl = row.inner.double_clicked();
        let resp = match text_resp { Some(t) => row.inner.union(t), None => row.inner };
        if self.select_mode && resp.clicked() {
            if !self.selection.remove(&m.id) { self.selection.insert(m.id.clone()); }
        }
        if dbl && !self.select_mode { actions.push(Action::Reply(m.id.clone())); }
        self.message_menu(&resp, core, m, mine, actions);
        if self.scroll_to.as_deref() == Some(m.id.as_str()) {
            ui.scroll_to_rect(bubble_rect, Some(Align::Center));
            self.scroll_to = None;
        }
        ui.add_space(2.0);
    }

    fn message_menu(&mut self, resp: &egui::Response, core: &Core, m: &ChatMessage, mine: bool, actions: &mut Vec<Action>) {
        let path = m.attachment.as_ref().filter(|a| a.is_available()).and_then(|a| core.attachment_path(a));
        resp.context_menu(|ui| {
            if ui.button("↩ Reply").clicked() { actions.push(Action::Reply(m.id.clone())); ui.close_menu(); }
            if m.content_type == CONTENT_TEXT && ui.button("📋 Copy text").clicked() { actions.push(Action::CopyText(m.text.clone())); ui.close_menu(); }
            if let Some(p) = &path {
                if ui.button("📂 Open").clicked() { actions.push(Action::Open(p.clone())); ui.close_menu(); }
                if ui.button("💾 Save as…").clicked() {
                    actions.push(Action::SaveAs(p.clone(), m.attachment.as_ref().map(|a| a.filename.clone()).unwrap_or_default()));
                    ui.close_menu();
                }
            }
            if mine && m.status == STATUS_FAILED && ui.button("⟳ Retry").clicked() { actions.push(Action::Resend(m.id.clone())); ui.close_menu(); }
            if ui.button("☑ Select").clicked() { self.select_mode = true; self.selection.insert(m.id.clone()); ui.close_menu(); }
            ui.separator();
            if ui.button("🗑 Delete for me").clicked() { actions.push(Action::Delete { ids: vec![m.id.clone()], everyone: false }); ui.close_menu(); }
            if ui.button(RichText::new("🗑 Delete for everyone").color(Color32::from_rgb(0xE0, 0x4F, 0x4F))).clicked() {
                actions.push(Action::Delete { ids: vec![m.id.clone()], everyone: true });
                ui.close_menu();
            }
        });
    }

    fn message_body(&mut self, ui: &mut egui::Ui, core: &Core, m: &ChatMessage, mine: bool, fg: Color32, actions: &mut Vec<Action>) -> Option<egui::Response> {
        match m.content_type {
            CONTENT_IMAGE => {
                let a = m.attachment.as_ref();
                let path = a.and_then(|a| core.attachment_path(a));
                let key = format!("img-{}", m.id);
                let p2 = path.clone();
                let inline = a.and_then(|a| a.data.clone());
                let tex = self.texture(ui.ctx(), &key, move || p2.and_then(|p| std::fs::read(p).ok()).or(inline), 640);
                match tex {
                    Some(t) => {
                        let s = t.size_vec2();
                        let scale = (300.0 / s.x).min(300.0 / s.y).min(1.0);
                        let r = ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), s * scale)).corner_radius(CornerRadius::same(8)).sense(Sense::click()));
                        if r.clicked() && !self.select_mode {
                            if let Some(p) = path { actions.push(Action::Open(p)); }
                        }
                        return Some(r.on_hover_cursor(egui::CursorIcon::PointingHand));
                    }
                    None => { ui.label("📷 Photo"); }
                }
            }
            CONTENT_FILE => {
                let Some(a) = m.attachment.as_ref() else { ui.label(&m.text); return None };
                let video = a.is_video();
                let mut thumb_resp: Option<egui::Response> = None;
                if let Some(th) = &a.thumbnail_data {
                    let key = format!("thumb-{}", m.id);
                    let b = th.clone();
                    if let Some(t) = self.texture(ui.ctx(), &key, move || Some(b), 400) {
                        let s = t.size_vec2();
                        let scale = (280.0 / s.x).min(220.0 / s.y).min(1.5);
                        let r = ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(t.id(), s * scale)).corner_radius(CornerRadius::same(8)).sense(Sense::click()));
                        if video { ui.painter().text(r.rect.center(), egui::Align2::CENTER_CENTER, "▶", egui::FontId::proportional(34.0), Color32::from_white_alpha(230)); }
                        if r.clicked() && a.is_available() && !self.select_mode { if let Some(p) = core.attachment_path(a) { actions.push(Action::Open(p)); } }
                        thumb_resp = Some(r);
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if video { "🎬" } else { "📄" }).size(26.0));
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(&a.filename).strong()).selectable(false));
                        let mut info = format_size(a.file_size);
                        if let Some(d) = a.duration { info = format!("{info} · {}:{:02}", (d as i64) / 60, (d as i64) % 60); }
                        ui.add(egui::Label::new(RichText::new(info).small()).selectable(false));
                    });
                });
                let progress = core.transfer_progress.get(&m.id).cloned();
                if let Some(p) = progress {
                    ui.add(egui::ProgressBar::new(p).desired_width(220.0).show_percentage());
                } else if !a.is_available() {
                    ui.label(RichText::new(if mine { "waiting…" } else { "waiting for sender…" }).small().italics());
                }
                if a.is_available() {
                    if let Some(p) = core.attachment_path(a) {
                        ui.horizontal(|ui| {
                            ui.style_mut().visuals.override_text_color = None;
                            if ui.small_button(if video { "▶ Play" } else { "📂 Open" }).clicked() { actions.push(Action::Open(p.clone())); }
                            if ui.small_button("💾 Save as…").clicked() { actions.push(Action::SaveAs(p.clone(), a.filename.clone())); }
                        });
                    }
                }
                return thumb_resp;
            }
            _ => {
                return Some(ui.add(egui::Label::new(RichText::new(&m.text).color(fg)).wrap()));
            }
        }
        None
    }
}

// ---------------- composer ----------------

impl App {
    fn composer(&mut self, ui: &mut egui::Ui, core: &Core, c: &Conversation, actions: &mut Vec<Action>) {
        if let Some(group) = &c.group {
            if !group.members.contains(&core.local_device_id) {
                ui.vertical_centered(|ui| ui.label(RichText::new("You are no longer a member of this group.").weak()));
                return;
            }
        }
        if let Some(rid) = self.reply_to.clone() {
            match c.messages.iter().find(|m| m.id == rid) {
                Some(m) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("↩").color(ACCENT).size(18.0));
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(RichText::new(format!("Reply to {}", core.sender_name(m, &c.id))).small().strong().color(ACCENT)).selectable(false));
                            ui.add(egui::Label::new(RichText::new(m.preview_text().replace('\n', " ")).small()).selectable(false).truncate());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("✕").clicked() { self.reply_to = None; }
                        });
                    });
                    ui.add_space(2.0);
                }
                None => self.reply_to = None,
            }
        }
        let cid = c.id.clone();
        ui.horizontal(|ui| {
            ui.menu_button(RichText::new("📎").size(18.0), |ui| {
                if ui.button("🖼 Photo or video…").clicked() { actions.push(Action::PickFiles { images_only: true }); ui.close_menu(); }
                if ui.button("📄 File…").clicked() { actions.push(Action::PickFiles { images_only: false }); ui.close_menu(); }
                if ui.button("📋 Paste image (Ctrl+V)").clicked() { actions.push(Action::PasteClipboard { from_menu: true }); ui.close_menu(); }
            }).response.on_hover_text("Attach");

            let send_w = 64.0;
            let width = (ui.available_width() - send_w - 8.0).max(80.0);
            let draft = self.draft.entry(cid.clone()).or_default();
            let before = draft.clone();
            let te = egui::TextEdit::multiline(draft)
                .id(egui::Id::new(("composer", &cid)))
                .hint_text("Message  (Enter to send, Shift+Enter for a new line)")
                .desired_rows(1)
                .desired_width(width)
                .lock_focus(true)
                .return_key(Some(egui::KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter)));
            let resp = egui::ScrollArea::vertical().id_salt(("composer_scroll", &cid)).max_height(140.0).show(ui, |ui| ui.add(te)).inner;
            if self.focus_composer { resp.request_focus(); self.focus_composer = false; }
            let changed = *draft != before;
            let enter = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt);
            let ime_busy = self.ime_composing || self.ime_commit_frame;
            let can_send = !draft.trim().is_empty();
            let clicked = ui.add_enabled(can_send, egui::Button::new(RichText::new("Send").color(if can_send { Color32::WHITE } else { ui.visuals().weak_text_color() })).fill(if can_send { ACCENT } else { ui.visuals().widgets.inactive.bg_fill }).min_size(Vec2::new(send_w, 32.0))).clicked();
            if (enter && !ime_busy || clicked) && can_send {
                let text = std::mem::take(draft);
                actions.push(Action::Send(text));
                if self.typing_active { actions.push(Action::Typing(cid.clone(), false)); self.typing_active = false; }
                self.focus_composer = true;
            } else if changed {
                self.last_draft_change = Some(Instant::now());
                let now_empty = draft.trim().is_empty();
                if now_empty && self.typing_active {
                    actions.push(Action::Typing(cid.clone(), false));
                    self.typing_active = false;
                } else if !now_empty && (!self.typing_active || self.last_typing_sent.map_or(true, |t| t.elapsed() > Duration::from_secs(3))) {
                    actions.push(Action::Typing(cid.clone(), true));
                    self.typing_active = true;
                    self.last_typing_sent = Some(Instant::now());
                }
            }
        });
        if self.typing_active && self.last_draft_change.map_or(true, |t| t.elapsed() > Duration::from_secs(4)) {
            actions.push(Action::Typing(cid, false));
            self.typing_active = false;
        } else if self.typing_active {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }
}

// ---------------- dialogs ----------------

fn modal_window(title: &str) -> egui::Window<'_> {
    egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
}

impl App {
    fn dialogs(&mut self, ctx: &egui::Context, core: &Core, actions: &mut Vec<Action>) {
        let danger = Color32::from_rgb(0xE0, 0x4F, 0x4F);

        // Connect by IP
        if self.dlg.connect {
            let mut open = true;
            modal_window("Connect by IP").open(&mut open).show(ctx, |ui| {
                ui.label("Enter the IP address shown in the other device's SecureChat settings.");
                ui.horizontal(|ui| {
                    ui.label("Address");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.dlg.connect_host).hint_text("192.168.1.23").desired_width(170.0));
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.dlg.connect_port).desired_width(60.0));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) { self.connect_from_dialog(actions); }
                });
                if let Some(e) = &core.last_manual_connect_error { ui.colored_label(danger, e); }
                if let Some(s) = &core.last_manual_connect_success { ui.colored_label(Color32::from_rgb(0x34, 0xA8, 0x53), s); }
                ui.horizontal(|ui| {
                    if ui.button("Connect").clicked() { self.connect_from_dialog(actions); }
                    if ui.button("Close").clicked() { self.dlg.connect = false; }
                });
            });
            if !open { self.dlg.connect = false; }
        }

        // New group
        if self.dlg.new_group {
            let mut open = true;
            let peers = core.known_peers();
            modal_window("New group").open(&mut open).show(ctx, |ui| {
                ui.horizontal(|ui| { ui.label("Name"); ui.text_edit_singleline(&mut self.dlg.group_name); });
                ui.label(RichText::new("Members").strong());
                if peers.is_empty() { ui.label(RichText::new("No contacts yet.").weak()); }
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for p in &peers {
                        let mut on = self.dlg.group_pick.contains(&p.id);
                        let online = core.is_connected(&p.id);
                        if ui.checkbox(&mut on, format!("{}{}", p.display_name, if online { "  ●" } else { "" })).changed() {
                            if on { self.dlg.group_pick.insert(p.id.clone()); } else { self.dlg.group_pick.remove(&p.id); }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let ok = !self.dlg.group_name.trim().is_empty() && !self.dlg.group_pick.is_empty();
                    if ui.add_enabled(ok, egui::Button::new("Create")).clicked() {
                        actions.push(Action::CreateGroup(self.dlg.group_name.trim().to_string(), self.dlg.group_pick.iter().cloned().collect()));
                        self.dlg.new_group = false;
                    }
                    if ui.button("Cancel").clicked() { self.dlg.new_group = false; }
                });
            });
            if !open { self.dlg.new_group = false; }
        }

        // Group info
        if self.dlg.group_info {
            let mut open = true;
            let conv = self.selected.as_ref().and_then(|id| core.conversations.get(id)).filter(|c| c.is_group());
            match conv {
                None => self.dlg.group_info = false,
                Some(c) => {
                    let g = c.group.clone().unwrap();
                    let me = core.local_device_id.clone();
                    let member = g.members.contains(&me);
                    modal_window("Group info").open(&mut open).show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.add_enabled(member, egui::TextEdit::singleline(&mut self.dlg.rename_buf));
                            let ok = member && !self.dlg.rename_buf.trim().is_empty() && self.dlg.rename_buf.trim() != g.name;
                            if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() {
                                actions.push(Action::RenameGroup(c.id.clone(), self.dlg.rename_buf.trim().to_string()));
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new(format!("{} members", g.members.len())).strong());
                        egui::ScrollArea::vertical().id_salt("gi_members").max_height(200.0).show(ui, |ui| {
                            for mid in &g.members {
                                ui.horizontal(|ui| {
                                    let name = if *mid == me { format!("{} (you)", core.display_name) } else { core.member_name(mid, Some(&c.id)) };
                                    let online = *mid == me || core.is_connected(mid);
                                    ui.label(RichText::new("●").color(if online { Color32::from_rgb(0x34, 0xC7, 0x59) } else { Color32::GRAY }));
                                    ui.label(name);
                                    if *mid == g.created_by { ui.label(RichText::new("creator").small().weak()); }
                                    if member && *mid != me {
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui.small_button("Remove").clicked() { actions.push(Action::RemoveMember(c.id.clone(), mid.clone())); }
                                        });
                                    }
                                });
                            }
                        });
                        if member {
                            let candidates: Vec<Peer> = core.known_peers().into_iter().filter(|p| !g.members.contains(&p.id)).collect();
                            if !candidates.is_empty() {
                                ui.separator();
                                ui.label(RichText::new("Add members").strong());
                                egui::ScrollArea::vertical().id_salt("gi_add").max_height(150.0).show(ui, |ui| {
                                    for p in &candidates {
                                        let mut on = self.dlg.add_pick.contains(&p.id);
                                        if ui.checkbox(&mut on, &p.display_name).changed() {
                                            if on { self.dlg.add_pick.insert(p.id.clone()); } else { self.dlg.add_pick.remove(&p.id); }
                                        }
                                    }
                                });
                                if ui.add_enabled(!self.dlg.add_pick.is_empty(), egui::Button::new("Add selected")).clicked() {
                                    actions.push(Action::AddMembers(c.id.clone(), self.dlg.add_pick.drain().collect()));
                                }
                            }
                            ui.separator();
                            if ui.button(RichText::new("Leave group").color(danger)).clicked() {
                                actions.push(Action::LeaveGroup(c.id.clone()));
                                self.dlg.group_info = false;
                            }
                        }
                    });
                }
            }
            if !open { self.dlg.group_info = false; }
        }

        // Settings
        if self.dlg.settings {
            let mut open = true;
            egui::Window::new("Profile & settings").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let me = core.local_device_id.clone();
                    self.avatar(ui, &me, &core.display_name, core.avatar_data.as_ref(), 64.0, None, false);
                    ui.vertical(|ui| {
                        if ui.button("Change photo…").clicked() { actions.push(Action::PickAvatar); }
                        if core.avatar_data.is_some() && ui.button("Remove photo").clicked() { actions.push(Action::RemoveAvatar); }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Display name");
                    ui.text_edit_singleline(&mut self.dlg.name_buf);
                    let ok = !self.dlg.name_buf.trim().is_empty() && self.dlg.name_buf.trim() != core.display_name;
                    if ui.add_enabled(ok, egui::Button::new("Save")).clicked() { actions.push(Action::SetName(self.dlg.name_buf.trim().to_string())); }
                });
                ui.label(RichText::new("Your name and photo are shown to the people you chat with.").small().weak());
                ui.separator();
                egui::Grid::new("settings_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Device ID"); ui.label(RichText::new(&core.local_device_id).monospace().small()); ui.end_row();
                    ui.label("Fingerprint"); ui.label(RichText::new(core.fingerprint()).monospace()); ui.end_row();
                    ui.label("Listening port");
                    ui.label(core.listening_port.map(|p| format!("TCP {p}")).unwrap_or_else(|| "not listening".into())); ui.end_row();
                    ui.label("This PC's IP");
                    let ips = securechat_core::net::own_addresses();
                    ui.label(if ips.is_empty() { "-".to_string() } else { ips.join(", ") }); ui.end_row();
                    ui.label("Data folder");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(core.store.dir.display().to_string()).small());
                        if ui.small_button("Open").clicked() { actions.push(Action::Open(core.store.dir.clone())); }
                    });
                    ui.end_row();
                });
                ui.separator();
                ui.label(RichText::new("Windows Firewall: when asked, allow SecureChat on Private networks (TCP 47823, UDP 47824) so phones and Macs can reach this PC.").small().weak());
            });
            if !open { self.dlg.settings = false; }
        }

        // Confirm delete chat
        if let Some(cid) = self.dlg.confirm_delete_chat.clone() {
            let name = core.conversations.get(&cid).map(|c| c.peer.display_name.clone()).unwrap_or_default();
            let group = core.conversations.get(&cid).map(|c| c.is_group()).unwrap_or(false);
            modal_window("Delete chat").show(ctx, |ui| {
                ui.label(if group { format!("Leave the group “{name}” and delete it from this PC?") } else { format!("Delete the chat with “{name}” and all its messages from this PC?") });
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Delete").color(danger)).clicked() { actions.push(Action::DeleteChat(cid.clone())); self.dlg.confirm_delete_chat = None; }
                    if ui.button("Cancel").clicked() { self.dlg.confirm_delete_chat = None; }
                });
            });
        }

        // Confirm clear chat
        if let Some(cid) = self.dlg.confirm_clear.clone() {
            modal_window("Clear chat").show(ctx, |ui| {
                ui.label("Delete all messages in this chat?");
                ui.checkbox(&mut self.dlg.clear_everyone, "Also delete for the other side");
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Clear").color(danger)).clicked() { actions.push(Action::ClearChat(cid.clone(), self.dlg.clear_everyone)); self.dlg.confirm_clear = None; }
                    if ui.button("Cancel").clicked() { self.dlg.confirm_clear = None; }
                });
            });
        }

        // Confirm delete selected
        if self.dlg.confirm_delete_selected {
            let n = self.selection.len();
            modal_window("Delete messages").show(ctx, |ui| {
                ui.label(format!("Delete {n} message(s)?"));
                ui.horizontal(|ui| {
                    if ui.button("Delete for me").clicked() {
                        actions.push(Action::Delete { ids: self.selection.iter().cloned().collect(), everyone: false });
                        self.dlg.confirm_delete_selected = false;
                    }
                    if ui.button(RichText::new("Delete for everyone").color(danger)).clicked() {
                        actions.push(Action::Delete { ids: self.selection.iter().cloned().collect(), everyone: true });
                        self.dlg.confirm_delete_selected = false;
                    }
                    if ui.button("Cancel").clicked() { self.dlg.confirm_delete_selected = false; }
                });
            });
        }

        // Paste preview
        if let Some((_, tex)) = &self.paste_preview {
            let tex = tex.clone();
            let mut send = false;
            let mut cancel = false;
            modal_window("Send image").show(ctx, |ui| {
                let s = tex.size_vec2();
                let scale = (420.0 / s.x).min(320.0 / s.y).min(1.0);
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), s * scale)).corner_radius(CornerRadius::same(6)));
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Send").color(Color32::WHITE)).fill(ACCENT)).clicked() { send = true; }
                    if ui.button("Cancel").clicked() { cancel = true; }
                });
                if ui.input(|i| i.key_pressed(Key::Enter)) { send = true; }
                if ui.input(|i| i.key_pressed(Key::Escape)) { cancel = true; }
            });
            if send { actions.push(Action::SendPastedImage); }
            if cancel { self.paste_preview = None; }
        }

        // Errors from the core
        if let Some(e) = &core.last_error {
            let since = self.toast.as_ref().filter(|(t, _)| t == e).map(|(_, i)| *i);
            let since = since.unwrap_or_else(|| { self.toast = Some((e.clone(), Instant::now())); Instant::now() });
            egui::Area::new(egui::Id::new("error_toast")).anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -90.0)).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).fill(Color32::from_rgb(0x44, 0x22, 0x22)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(e).color(Color32::WHITE));
                        if ui.small_button("✕").clicked() { actions.push(Action::ClearError); }
                    });
                });
            });
            if since.elapsed() > Duration::from_secs(6) { actions.push(Action::ClearError); }
            else { ctx.request_repaint_after(Duration::from_millis(500)); }
        }
    }

    fn connect_from_dialog(&mut self, actions: &mut Vec<Action>) {
        let host = self.dlg.connect_host.trim().to_string();
        if host.is_empty() { return; }
        let port = self.dlg.connect_port.trim().parse::<u16>().unwrap_or(47823);
        actions.push(Action::ConnectIp(host, port));
    }
}

// ---------------- frame + actions ----------------

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // IME tracking (Enter that commits a composition must not send).
        self.ime_commit_frame = false;
        let mut dropped: Vec<PathBuf> = vec![];
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Ime(ime) = e {
                    match ime {
                        egui::ImeEvent::Preedit(t) => self.ime_composing = !t.is_empty(),
                        egui::ImeEvent::Commit(_) => { self.ime_composing = false; self.ime_commit_frame = true; }
                        egui::ImeEvent::Disabled => self.ime_composing = false,
                        egui::ImeEvent::Enabled => {}
                    }
                }
            }
            for f in &i.raw.dropped_files {
                if let Some(p) = &f.path { dropped.push(p.clone()); }
            }
        });

        let mut actions: Vec<Action> = vec![];
        if !dropped.is_empty() { actions.push(Action::DroppedFiles(dropped)); }

        // Ctrl+V: egui only sees text pastes, so poll the V key for image/file clipboards.
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        let v = ctrl && v_key_down();
        if v && !self.v_was_down && self.selected.is_some() && ctx.input(|i| i.focused) {
            actions.push(Action::PasteClipboard { from_menu: false });
        }
        self.v_was_down = v;
        if ctrl { ctx.request_repaint_after(Duration::from_millis(50)); }

        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.select_mode { self.select_mode = false; self.selection.clear(); }
            else if self.reply_to.is_some() { self.reply_to = None; }
        }

        let h = self.h.clone();
        {
            let core = h.lock();
            if let Some(sel) = &self.selected {
                if !core.conversations.contains_key(sel) { self.selected = None; }
            }
            egui::SidePanel::left("chats").resizable(true).default_width(300.0).width_range(240.0..=460.0).show(ctx, |ui| {
                self.chat_list(ui, &core, &mut actions);
            });
            egui::CentralPanel::default().frame(egui::Frame::central_panel(&ctx.style()).inner_margin(0.0)).show(ctx, |ui| {
                match self.selected.clone() {
                    Some(cid) => {
                        self.conversation(ui, &core, &cid, &mut actions);
                        if let Some(c) = core.conversations.get(&cid) {
                            if c.unread_count > 0 && ctx.input(|i| i.focused) { actions.push(Action::MarkRead(cid.clone())); }
                        }
                    }
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label(RichText::new("Select a chat to start messaging\n\n🔒 Messages are end-to-end encrypted").weak());
                        });
                    }
                }
            });
            if ctx.input(|i| !i.raw.hovered_files.is_empty()) && self.selected.is_some() {
                egui::Area::new(egui::Id::new("drop_overlay")).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| { ui.label(RichText::new("Drop files to send").size(20.0)); });
                });
            }
            self.dialogs(ctx, &core, &mut actions);
        }
        if !actions.is_empty() { self.apply(ctx, actions); }
        // New windows need a second pass to size themselves; repaint right away.
        let sig = (self.dlg.connect, self.dlg.new_group, self.dlg.group_info, self.dlg.settings,
            self.dlg.confirm_delete_chat.is_some(), self.dlg.confirm_clear.is_some(), self.dlg.confirm_delete_selected, self.paste_preview.is_some());
        let sig = format!("{sig:?}");
        if sig != self.dlg_sig { self.dlg_sig = sig; ctx.request_repaint(); }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let mut core = self.h.lock();
        core.flush();
        core.shutdown();
    }
}

fn pick_image_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("Images", &["jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "tif", "tiff"])
}

impl App {
    fn send_paths(&mut self, cid: &str, paths: Vec<PathBuf>) {
        let reply = self.reply_to.take();
        let mut core = self.h.lock();
        for p in paths {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
            let lower = name.to_lowercase();
            let as_photo = securechat_core::media::is_image_name(&name) && !lower.ends_with(".gif") && !lower.ends_with(".heic");
            if p.is_dir() { core.last_error = Some(format!("{name} is a folder – folders can't be sent")); continue; }
            if as_photo {
                match std::fs::read(&p) {
                    Ok(b) if b.len() < 60 * 1024 * 1024 => match securechat_core::media::prepare_photo_bytes(&b) {
                        Ok(prep) => core.send_prepared_image(prep, &name, cid, reply.as_deref()),
                        Err(_) => core.send_file(&p, cid, reply.as_deref()),
                    },
                    _ => core.send_file(&p, cid, reply.as_deref()),
                }
            } else {
                core.send_file(&p, cid, reply.as_deref());
            }
        }
        self.scroll_bottom = true;
    }

    fn paste(&mut self, ctx: &egui::Context, from_menu: bool) {
        let Ok(mut cb) = arboard::Clipboard::new() else { return };
        // Ctrl+V with text on the clipboard is handled by the text box itself.
        if !from_menu && cb.get_text().map(|t| !t.is_empty()).unwrap_or(false) { return; }
        if let Ok(img) = cb.get_image() {
            let (w, hgt) = (img.width as u32, img.height as u32);
            if let Some(dynimg) = securechat_core::media::from_rgba(w, hgt, img.bytes.into_owned()) {
                match securechat_core::media::prepare_photo(&dynimg) {
                    Ok(prep) => {
                        if let Some(tex) = texture_from_bytes(ctx, "paste-preview", &prep.jpeg, 900) {
                            self.paste_preview = Some((prep, tex));
                        }
                    }
                    Err(e) => self.h.lock().last_error = Some(format!("Could not paste image: {e}")),
                }
            }
            return;
        }
        if let Ok(files) = cb.get().file_list() {
            if !files.is_empty() {
                if let Some(cid) = self.selected.clone() { self.send_paths(&cid, files); }
                return;
            }
        }
        if from_menu {
            match cb.get_text() {
                Ok(t) if !t.is_empty() => {
                    if let Some(cid) = &self.selected { self.draft.entry(cid.clone()).or_default().push_str(&t); }
                }
                _ => self.h.lock().last_error = Some("The clipboard has no image".into()),
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        let cid = self.selected.clone();
        for a in actions {
            match a {
                Action::Send(text) => {
                    if let Some(c) = &cid {
                        let reply = self.reply_to.take();
                        self.h.lock().send_message(&text, c, reply.as_deref());
                        self.scroll_bottom = true;
                    }
                }
                Action::PickFiles { images_only } => {
                    let Some(c) = cid.clone() else { continue };
                    let dlg = if images_only {
                        rfd::FileDialog::new().add_filter("Photos & videos", &["jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "tif", "tiff", "mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "3gp"])
                    } else { rfd::FileDialog::new() };
                    if let Some(paths) = dlg.set_title("Send").pick_files() { self.send_paths(&c, paths); }
                }
                Action::PasteClipboard { from_menu } => self.paste(ctx, from_menu),
                Action::SendPastedImage => {
                    if let (Some(c), Some((prep, _))) = (cid.clone(), self.paste_preview.take()) {
                        let reply = self.reply_to.take();
                        let name = format!("Screenshot {}.jpg", chrono::Local::now().format("%Y-%m-%d %H.%M.%S"));
                        self.h.lock().send_prepared_image(prep, &name, &c, reply.as_deref());
                        self.scroll_bottom = true;
                    }
                }
                Action::DroppedFiles(paths) => { if let Some(c) = cid.clone() { self.send_paths(&c, paths); } }
                Action::Reply(id) => { self.reply_to = Some(id); self.focus_composer = true; }
                Action::CopyText(t) => ctx.copy_text(t),
                Action::Delete { ids, everyone } => {
                    if let Some(c) = &cid { self.h.lock().delete_messages(&ids, c, everyone); }
                    if self.reply_to.as_ref().is_some_and(|r| ids.contains(r)) { self.reply_to = None; }
                    self.select_mode = false;
                    self.selection.clear();
                }
                Action::Resend(id) => { if let Some(c) = &cid { self.h.lock().resend(&id, c); } }
                Action::Open(p) => {
                    if let Err(e) = open::that_detached(&p) { self.h.lock().last_error = Some(format!("Could not open: {e}")); }
                }
                Action::SaveAs(p, name) => {
                    if let Some(dest) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                        if let Err(e) = std::fs::copy(&p, &dest) { self.h.lock().last_error = Some(format!("Save failed: {e}")); }
                    }
                }
                Action::MarkRead(c) => self.h.lock().mark_as_read(&c),
                Action::Scan => self.h.lock().scan_now(),
                Action::ConnectIp(host, port) => self.h.lock().connect_by_ip(&host, port),
                Action::CreateGroup(name, members) => {
                    let gid = self.h.lock().create_group(&name, &members);
                    self.selected = Some(gid);
                    self.scroll_bottom = true;
                }
                Action::RenameGroup(g, n) => self.h.lock().rename_group(&g, &n),
                Action::AddMembers(g, ids) => self.h.lock().add_members(&ids, &g),
                Action::RemoveMember(g, id) => self.h.lock().remove_member(&id, &g),
                Action::LeaveGroup(g) => self.h.lock().leave_group(&g),
                Action::ClearChat(c, everyone) => { self.h.lock().clear_chat(&c, everyone); self.reply_to = None; }
                Action::DeleteChat(c) => {
                    self.h.lock().delete_conversation(&c);
                    if self.selected.as_deref() == Some(c.as_str()) { self.selected = None; }
                    self.draft.remove(&c);
                }
                Action::SetName(n) => self.h.lock().update_display_name(&n),
                Action::PickAvatar => {
                    if let Some(p) = pick_image_dialog().set_title("Choose a profile photo").pick_file() {
                        let res = std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| securechat_core::media::prepare_avatar(&b));
                        let mut core = self.h.lock();
                        match res {
                            Ok(j) => core.update_avatar(Some(j)),
                            Err(e) => core.last_error = Some(format!("Could not use that image: {e}")),
                        }
                    }
                }
                Action::RemoveAvatar => self.h.lock().update_avatar(None),
                Action::Typing(c, on) => self.h.lock().send_typing(&c, on),
                Action::ClearError => { self.h.lock().last_error = None; self.toast = None; }
            }
        }
        self.h.wake();
        ctx.request_repaint();
    }
}
