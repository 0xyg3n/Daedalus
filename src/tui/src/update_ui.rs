//! Launch, exit and manual update checks with explicit consent. Workers never own UI state.
use crate::theme::{Theme, Tone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use daedalus_tui::app::update::{self, InstallReceipt, UpdateReview, UpdateStatus};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
#[cfg(any(windows, test))]
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

type Check = fn() -> Result<UpdateStatus, String>;

enum Completion {
    Check(Result<Box<UpdateStatus>, String>),
    Install(Result<InstallReceipt, String>),
}
pub struct Controller {
    worker: Option<Receiver<Completion>>,
    automatic: bool,
    exiting: bool,
    quit: bool,
    offered_version: Option<String>,
    banner: Option<String>,
    installed: bool,
    install_result: bool,
    review: Option<UpdateReview>,
    notice: String,
    visible: bool,
    pending_prompt: bool,
    update_selected: bool,
    installing: bool,
    body_scroll: u16,
    #[cfg(any(windows, test))]
    pending_installer: Option<PathBuf>,
    later_hit: Rect,
    update_hit: Rect,
}
impl Controller {
    pub fn new(enabled: bool) -> Self {
        let mut this = Self {
            worker: None,
            automatic: enabled,
            exiting: false,
            quit: false,
            offered_version: None,
            banner: None,
            installed: false,
            install_result: false,
            review: None,
            notice: if enabled {
                "Checking for updates…"
            } else {
                "Automatic update checks are disabled."
            }
            .into(),
            visible: false,
            pending_prompt: false,
            update_selected: false,
            installing: false,
            body_scroll: 0,
            #[cfg(any(windows, test))]
            pending_installer: None,
            later_hit: Rect::default(),
            update_hit: Rect::default(),
        };
        if enabled {
            this.start_check(update::check);
        }
        this
    }
    fn start_check(&mut self, check: Check) {
        if self.worker.is_some() || self.installing || self.installed {
            return;
        }
        self.install_result = false;
        self.review = None;
        self.banner = None;
        self.notice = "Checking for updates…".into();
        self.body_scroll = 0;
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Completion::Check(check().map(Box::new)));
        });
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    /// Caller must defer opening while another dialog, embedded terminal or splash owns input.
    pub fn open(&mut self) {
        self.open_with(update::check);
    }
    fn open_with(&mut self, check: Check) {
        self.show();
        // Keep installation results available until reviewed or explicitly retried.
        if self.install_result {
            return;
        }
        self.start_check(check);
    }
    fn show(&mut self) {
        self.visible = true;
        self.pending_prompt = false;
        self.update_selected = false;
    }
    pub fn poll(&mut self, allow_prompt: bool) {
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(result) => {
                    self.worker = None;
                    self.complete(result);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    let was_installing = self.installing;
                    self.installing = false;
                    self.review = None;
                    self.notice = "Update worker stopped unexpectedly. Check the installed version before retrying.".into();
                    self.banner = Some(self.notice.clone());
                    self.install_result = was_installing;
                    self.pending_prompt = was_installing && !self.visible;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if allow_prompt && self.pending_prompt {
            self.show();
        }
    }
    pub fn request_exit(&mut self) {
        self.exit_with(update::check);
    }
    fn exit_with(&mut self, check: Check) {
        if (!self.automatic && !self.installing) || self.installed {
            self.quit = true;
            return;
        }
        self.exiting = true;
        self.show();
        self.start_check(check);
    }
    pub fn should_quit(&self) -> bool {
        self.quit
    }
    fn dismiss(&mut self) {
        // A download must finish before exit; Escape returns to the interface.
        self.quit = self.exiting && !self.installing;
        self.exiting = false;
        self.visible = false;
    }
    fn complete(&mut self, result: Completion) {
        match result {
            Completion::Check(Ok(status)) => {
                self.notice = if status.status == "up_to_date" {
                    format!(
                        "You’re up to date. Daedalus {} is installed.",
                        status.current_version
                    )
                } else {
                    format!("{} (installed {})", status.message, status.current_version)
                };
                self.pending_prompt =
                    status.review.as_ref().is_some_and(|review| {
                        self.offered_version.as_ref() != Some(&review.version)
                    }) && !self.visible;
                if let Some(review) = &status.review {
                    self.offered_version = Some(review.version.clone());
                }
                self.review = status.review;
                self.banner = None;
                if self.exiting && self.review.is_none() {
                    self.quit = true;
                }
            }
            Completion::Check(Err(error)) => {
                self.review = None;
                self.notice = format!("Update check failed: {error}");
                self.banner = Some("Update check failed · Ctrl+U to retry".into());
            }
            Completion::Install(result) => {
                self.install_result = true;
                self.installing = false;
                self.review = None;
                self.notice = match result {
                    Ok(receipt) => {
                        if receipt.installer_ready {
                            #[cfg(any(windows, test))]
                            {
                                self.pending_installer =
                                    Some(PathBuf::from(&receipt.installed_path));
                            }
                            self.quit = true;
                        }
                        // This process still runs the old executable. Keep the receipt
                        // until restart instead of offering the same update again.
                        self.installed = true;
                        self.banner = Some(if receipt.installer_ready {
                            format!("Installing {} · closing Daedalus", receipt.version)
                        } else if receipt.activated {
                            format!(
                                "Updated to {} · Restart Daedalus to use it",
                                receipt.version
                            )
                        } else {
                            format!(
                                "Downloaded {} · Manual activation required",
                                receipt.version
                            )
                        });
                        format!(
                            "{} {}. {}{}",
                            if receipt.installer_ready {
                                "Starting installer for"
                            } else if receipt.activated {
                                "Updated to"
                            } else {
                                "Downloaded"
                            },
                            receipt.version,
                            receipt.message,
                            if receipt.restart_required && receipt.activated {
                                " Restart Daedalus when ready to use the new version; your terminals stay open."
                            } else {
                                ""
                            }
                        )
                    }
                    Err(error) => format!(
                        "Update failed: {error}. Check the installed version before retrying."
                    ),
                };
                if !self.installed {
                    self.banner = Some("Update failed · Ctrl+U details".into());
                }
                self.pending_prompt = !self.visible;
            }
        }
    }
    #[cfg(any(windows, test))]
    pub fn take_installer(&mut self) -> Option<PathBuf> {
        self.pending_installer.take()
    }
    fn consent(&mut self) -> Option<UpdateReview> {
        if !self.visible || self.installing {
            return None;
        }
        let review = self.review.take()?;
        self.installing = true;
        self.exiting = false;
        self.update_selected = false;
        self.notice = format!("Preparing verified update {}…", review.version);
        Some(review)
    }
    fn install(&mut self) {
        if let Some(review) = self.consent() {
            let (tx, rx) = mpsc::channel();
            self.worker = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(Completion::Install(update::install(
                    &review.version,
                    &review.approval_sha256,
                )));
            });
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if !self.visible {
            return false;
        }
        if key.kind != KeyEventKind::Press {
            return true;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('n' | 'N') => self.dismiss(),
            KeyCode::Char('y' | 'Y') => self.install(),
            KeyCode::Char('r' | 'R') => self.start_check(update::check),
            KeyCode::Up => self.body_scroll = self.body_scroll.saturating_sub(1),
            KeyCode::Down => self.body_scroll = self.body_scroll.saturating_add(1),
            KeyCode::PageUp => self.body_scroll = self.body_scroll.saturating_sub(8),
            KeyCode::PageDown => self.body_scroll = self.body_scroll.saturating_add(8),
            KeyCode::Home => self.body_scroll = 0,
            KeyCode::End => self.body_scroll = u16::MAX,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab if self.review.is_some() => {
                self.update_selected = !self.update_selected;
            }
            KeyCode::Enter if self.update_selected && self.review.is_some() => self.install(),
            KeyCode::Enter => self.dismiss(),
            _ => {}
        }
        true
    }
    pub fn mouse(&mut self, mouse: MouseEvent) -> bool {
        if !self.visible {
            return false;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let point = ratatui::layout::Position::new(mouse.column, mouse.row);
            if self.later_hit.contains(point) {
                self.dismiss();
            } else if self.update_hit.contains(point) {
                if self.review.is_some() {
                    self.install();
                } else {
                    self.start_check(update::check);
                }
            }
        }
        true
    }
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        self.later_hit = Rect::default();
        self.update_hit = Rect::default();
        if area.width < 12 || area.height < 3 {
            return;
        }
        if !self.visible {
            if self.review.is_some() || self.installing || self.banner.is_some() {
                let text = if self.installing {
                    "Updating… · Ctrl+U details".to_owned()
                } else if let Some(review) = &self.review {
                    format!("Update {} available · Ctrl+U", review.version)
                } else {
                    let banner = self.banner.as_deref().unwrap_or_default();
                    if banner.contains("Ctrl+U") {
                        banner.to_owned()
                    } else {
                        format!("{banner} · Ctrl+U details")
                    }
                };
                let toast = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
                frame.render_widget(Paragraph::new(text).style(theme.fg(Tone::Accent)), toast);
            }
            return;
        }
        let width = area.width.min(76);
        let height = area.height.min(18);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(if self.exiting {
                " Check for updates before exit "
            } else {
                " Daedalus updates "
            })
            .borders(Borders::ALL)
            .border_set(theme.border())
            .border_style(theme.fg(Tone::Accent))
            .style(theme.surface());
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let detail = match &self.review {
            Some(review) => format!(
                "{}\n\nRelease: {}\nPlatform: {}\nDownload: {}\nInstall: {}\n\nYour terminals stay open. Installation starts only after you choose Update.",
                self.notice,
                review.version,
                review.platform,
                download_size(review.size),
                if review.platform.ends_with("windows") {
                    "Current-user Windows installation"
                } else {
                    &review.install_path
                }
            ),
            None => self.notice.clone(),
        };
        let version = format!(" Installed {}", env!("CARGO_PKG_VERSION"));
        frame.render_widget(
            Paragraph::new(Span::styled(
                crate::ui_text::fit(&version, inner.width as usize, theme),
                theme.fg(Tone::Muted),
            )),
            Rect::new(inner.x, inner.y, inner.width, 1.min(inner.height)),
        );
        let body = Rect::new(
            inner.x,
            (inner.y + 2).min(inner.bottom()),
            inner.width,
            inner.height.saturating_sub(4),
        );
        let lines = crate::ui_text::wrap(&detail, body.width.saturating_sub(2) as usize)
            .into_iter()
            .map(|line| {
                if let Some((label, value)) = line.split_once(": ")
                    && matches!(label, "Release" | "Platform" | "Download" | "Install")
                {
                    Line::from(vec![
                        Span::styled(format!(" {label}: "), theme.fg(Tone::Info)),
                        Span::styled(value.to_owned(), theme.fg(Tone::Text)),
                    ])
                } else {
                    Line::from(Span::styled(format!(" {line}"), theme.fg(Tone::Text)))
                }
            })
            .collect::<Vec<_>>();
        let most = lines
            .len()
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        self.body_scroll = self.body_scroll.min(most);
        frame.render_widget(Paragraph::new(lines).scroll((self.body_scroll, 0)), body);
        if most > 0 && inner.height >= 3 {
            let hint = format!(
                " {} Scroll details · {}–{} / {}",
                theme.glyph("↑↓", "Up/Dn"),
                self.body_scroll + 1,
                (self.body_scroll as usize + body.height as usize)
                    .min(most as usize + body.height as usize),
                most as usize + body.height as usize
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::ui_text::fit(&hint, inner.width as usize, theme),
                    theme.fg(Tone::Muted),
                )),
                Rect::new(inner.x, inner.bottom() - 2, inner.width, 1),
            );
        }
        if inner.height >= 2 {
            self.later_hit = Rect::new(inner.x, inner.bottom() - 1, inner.width.min(17), 1);
            let later = if self.exiting && !self.installing {
                " Exit [Esc] "
            } else if self.review.is_some() {
                " Later [Esc] "
            } else {
                " Close [Esc] "
            };
            let active = theme.fg(Tone::Accent);
            let inactive = theme.fg(Tone::Muted);
            let later_style = if self.update_selected {
                inactive
            } else {
                active
            };
            frame.render_widget(Paragraph::new(later).style(later_style), self.later_hit);
            if self.review.is_none() && !self.installing && !self.installed && inner.width > 18 {
                self.update_hit = Rect::new(inner.x + 18, inner.bottom() - 1, inner.width - 18, 1);
                frame.render_widget(
                    Paragraph::new(" Recheck [R] ").style(inactive),
                    self.update_hit,
                );
            }
            if self.review.is_some() && inner.width > 18 {
                self.update_hit = Rect::new(inner.x + 18, inner.bottom() - 1, inner.width - 18, 1);
                let style = if self.update_selected {
                    active
                } else {
                    inactive
                };
                frame.render_widget(Paragraph::new(" Update [Y] ").style(style), self.update_hit);
            }
        }
    }
}

fn download_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }
    #[test]
    fn long_update_errors_can_be_read_to_the_end_without_starting_an_update() {
        let mut ui = Controller::new(false);
        ui.show();
        ui.notice = format!(
            "{}END-OF-DETAILS",
            "A detailed diagnostic line. ".repeat(80)
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(44, 14)).unwrap();
        let render =
            |ui: &mut Controller,
             terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                terminal
                    .draw(|f| ui.draw(f, f.area(), &Theme::default()))
                    .unwrap();
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            };
        let first = render(&mut ui, &mut terminal);
        assert!(first.contains("Installed"));
        assert!(first.contains("Scroll details"));
        assert!(!first.contains("END-OF-DETAILS"));
        ui.key(key(KeyCode::End));
        let last = render(&mut ui, &mut terminal);
        assert!(last.contains("END-OF-DETAILS"));
        assert!(!ui.installing);
        ui.key(key(KeyCode::Home));
        assert_eq!(ui.body_scroll, 0);
        assert_eq!(download_size(1024 * 1024 + 512 * 1024), "1.5 MiB");
    }
    #[test]
    fn failed_update_toast_has_one_details_shortcut() {
        let mut ui = Controller::new(false);
        ui.complete(Completion::Check(Err("offline".into())));
        let text = screen(&mut ui);
        assert_eq!(text.matches("Ctrl+U").count(), 1);
    }
    fn available() -> Completion {
        Completion::Check(Ok(serde_json::from_value(serde_json::json!({
            "status":"available", "current_version":"1.0.0", "latest_version":"1.1.0",
            "release_url":"https://example.invalid", "message":"New release available",
            "review":{"version":"1.1.0", "platform":"test", "asset_name":"test",
                "asset_url":"https://example.invalid/test", "sha256":"a".repeat(64),
                "current_sha256":"b".repeat(64), "size":100, "install_path":"test",
                "approval_sha256":"c".repeat(64)}
        }))
        .unwrap()))
    }

    #[test]
    fn disabled_and_deferred_checks_never_capture_terminal_input() {
        let mut ui = Controller::new(false);
        assert!(ui.worker.is_none());
        ui.complete(available());
        ui.poll(false);
        assert!(!ui.key(key(KeyCode::Char('y'))));
        assert!(!ui.installing);
        ui.poll(true);
        assert!(ui.visible());
        ui.key(key(KeyCode::Enter));
        assert!(!ui.visible());
        assert!(ui.review.is_some());
        ui.poll(true);
        assert!(!ui.visible());
    }
    #[test]
    fn consent_is_exact_single_use_and_preserves_receipt() {
        let mut ui = Controller::new(false);
        ui.complete(available());
        assert!(ui.consent().is_none());
        ui.show();
        let review = ui.consent().unwrap();
        assert_eq!(review.approval_sha256, "c".repeat(64));
        assert!(ui.consent().is_none());
        ui.complete(Completion::Install(Ok(InstallReceipt {
            version: "1.1.0".into(),
            installed_path: "test".into(),
            previous_path: "old".into(),
            restart_required: true,
            activated: false,
            installer_ready: false,
            message: "Staged".into(),
        })));
        assert!(ui.notice.contains("Downloaded 1.1.0"));
        assert!(!ui.installing);
    }
    #[test]
    fn verified_windows_installer_closes_tui_for_activation() {
        let mut ui = Controller::new(false);
        ui.complete(Completion::Install(Ok(InstallReceipt {
            version: "1.1.0".into(),
            installed_path: "C:/private/Daedalus-setup.exe".into(),
            previous_path: "C:/private/previous.exe".into(),
            restart_required: true,
            activated: false,
            installer_ready: true,
            message: "Verified installer ready".into(),
        })));
        assert!(ui.should_quit());
        assert_eq!(
            ui.take_installer(),
            Some(PathBuf::from("C:/private/Daedalus-setup.exe"))
        );
        assert!(ui.take_installer().is_none());
    }
    #[test]
    fn errors_and_current_release_are_visible_without_install_offer() {
        let mut ui = Controller::new(false);
        ui.complete(Completion::Check(Err("offline".into())));
        ui.show();
        assert!(ui.notice.contains("offline"));
        assert!(ui.consent().is_none());
        let Completion::Check(Ok(mut status)) = available() else {
            unreachable!()
        };
        status.review = None;
        status.status = "no_release".into();
        status.message = "No compatible release".into();
        ui.complete(Completion::Check(Ok(status)));
        assert!(ui.notice.contains("No compatible release"));
        assert!(ui.consent().is_none());
    }
    fn check_available() -> Result<UpdateStatus, String> {
        let Completion::Check(result) = available() else {
            unreachable!()
        };
        result.map(|status| *status)
    }
    fn no_network() -> Result<UpdateStatus, String> {
        panic!("Unexpected release request")
    }
    fn drain(ui: &mut Controller, allow_prompt: bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while ui.worker.is_some() {
            ui.poll(allow_prompt);
            assert!(Instant::now() < deadline, "update worker timed out");
            std::thread::yield_now();
        }
    }
    fn screen(ui: &mut Controller) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 25)).unwrap();
        terminal
            .draw(|frame| ui.draw(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
    #[test]
    fn manual_open_checks_again_after_startup_was_current_or_offline() {
        let mut ui = Controller::new(false);
        ui.notice = "Already up to date".into();
        ui.open_with(check_available);
        assert!(screen(&mut ui).contains("Checking for updates"));
        drain(&mut ui, true);
        let rendered = screen(&mut ui);
        assert!(rendered.contains("Release: 1.1.0"));
        assert!(rendered.contains("Update [Y]"));
        assert!(!ui.update_selected);
        ui.key(key(KeyCode::Enter)); // Default is Later, never installs.
        assert!(!ui.visible && !ui.installing);
        assert!(screen(&mut ui).contains("Update 1.1.0 available"));

        ui.complete(Completion::Check(Err("offline".into())));
        assert!(screen(&mut ui).contains("Ctrl+U to retry"));
        ui.open_with(check_available);
        drain(&mut ui, true);
        assert!(screen(&mut ui).contains("Update [Y]"));
    }
    #[test]
    fn exit_checks_again_and_offers_update_or_exit() {
        let mut ui = Controller::new(false);
        ui.automatic = true;
        ui.exit_with(check_available);
        assert!(ui.visible && !ui.should_quit());
        assert!(screen(&mut ui).contains("Check for updates before exit"));
        drain(&mut ui, true);
        assert!(screen(&mut ui).contains("Update [Y]"));
        assert!(screen(&mut ui).contains("Exit [Esc]"));
        ui.key(key(KeyCode::Enter));
        assert!(ui.should_quit());
        assert!(!ui.installing);
    }
    #[test]
    fn exit_respects_disabled_checks_and_does_not_install_implicitly() {
        let mut ui = Controller::new(false);
        ui.exit_with(no_network);
        assert!(ui.should_quit());
        let mut ui = Controller::new(false);
        ui.automatic = true;
        ui.exit_with(check_available);
        drain(&mut ui, true);
        assert!(ui.consent().is_some());
        assert!(!ui.exiting && !ui.should_quit());
        ui.key(key(KeyCode::Esc));
        assert!(
            !ui.should_quit(),
            "installation stays running after dialog closes"
        );
    }
    #[test]
    fn exit_check_is_skippable_and_current_release_exits_without_a_prompt() {
        let mut ui = Controller::new(false);
        ui.automatic = true;
        ui.exit_with(check_available);
        ui.key(key(KeyCode::Esc));
        assert!(ui.should_quit());
        drain(&mut ui, false);
        let mut ui = Controller::new(false);
        ui.exiting = true;
        let mut status = check_available().unwrap();
        status.review = None;
        status.status = "up_to_date".into();
        ui.complete(Completion::Check(Ok(Box::new(status))));
        assert!(ui.should_quit());
        assert!(ui.notice.contains("up to date"));
    }
    #[test]
    fn closing_during_install_still_shows_completion_and_keeps_receipt() {
        let mut ui = Controller::new(false);
        ui.complete(available());
        ui.show();
        ui.consent().unwrap();
        ui.key(key(KeyCode::Esc));
        let (tx, rx) = mpsc::channel();
        ui.worker = Some(rx);
        tx.send(Completion::Install(Ok(InstallReceipt {
            version: "1.1.0".into(),
            installed_path: "test".into(),
            previous_path: "old".into(),
            restart_required: true,
            activated: true,
            installer_ready: false,
            message: "Verified executable installed.".into(),
        })))
        .unwrap();
        drain(&mut ui, false); // Attached terminal owns input.
        assert!(!ui.visible);
        assert!(screen(&mut ui).contains("Updated to 1.1.0"));
        ui.poll(true);
        assert!(ui.visible);
        assert!(screen(&mut ui).contains("Restart Daedalus"));
        ui.key(key(KeyCode::Esc));
        ui.automatic = true;
        ui.poll(true);
        ui.open_with(no_network);
        assert!(screen(&mut ui).contains("Updated to 1.1.0"));
        assert!(!screen(&mut ui).contains("Update [Y]"));
    }
    #[test]
    fn failed_or_disconnected_install_is_reported_after_closing_dialog() {
        for disconnected in [false, true] {
            let mut ui = Controller::new(false);
            ui.installing = true;
            let (tx, rx) = mpsc::channel();
            ui.worker = Some(rx);
            if !disconnected {
                tx.send(Completion::Install(Err("checksum mismatch".into())))
                    .unwrap();
            }
            drop(tx);
            drain(&mut ui, false);
            assert!(!ui.visible && !ui.installing);
            ui.open_with(no_network);
            assert!(screen(&mut ui).contains(if disconnected {
                "worker stopped unexpectedly"
            } else {
                "checksum mismatch"
            }));
            assert!(screen(&mut ui).contains("Recheck [R]"));
            assert!(ui.consent().is_none());
        }
    }
}
