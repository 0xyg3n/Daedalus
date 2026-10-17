//! Optional launch check and explicit, reviewed update consent. Workers never own UI state.
use crate::theme::{Theme, Tone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use daedalus_tui::app::update::{self, InstallReceipt, UpdateReview, UpdateStatus};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::sync::mpsc::{self, Receiver};

enum Completion {
    Check(Result<Box<UpdateStatus>, String>),
    Install(Result<InstallReceipt, String>),
}
pub struct Controller {
    worker: Option<Receiver<Completion>>,
    review: Option<UpdateReview>,
    notice: String,
    visible: bool,
    pending_prompt: bool,
    update_selected: bool,
    installing: bool,
    later_hit: Rect,
    update_hit: Rect,
}
impl Controller {
    pub fn new(enabled: bool) -> Self {
        let mut this = Self {
            worker: None,
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
            later_hit: Rect::default(),
            update_hit: Rect::default(),
        };
        if enabled {
            let (tx, rx) = mpsc::channel();
            this.worker = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(Completion::Check(update::check().map(Box::new)));
            });
        }
        this
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    /// Caller must defer opening while another dialog, embedded terminal or splash owns input.
    pub fn open(&mut self) {
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
                    self.installing = false;
                    self.review = None;
                    self.notice = "Update worker stopped unexpectedly. Check the installed version before retrying.".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if allow_prompt && self.pending_prompt {
            self.open();
        }
    }
    fn complete(&mut self, result: Completion) {
        match result {
            Completion::Check(Ok(status)) => {
                self.notice = format!("{} (installed {})", status.message, status.current_version);
                self.review = status.review;
                self.pending_prompt = self.review.is_some() && !self.visible;
            }
            Completion::Check(Err(error)) => self.notice = format!("Update check failed: {error}"),
            Completion::Install(result) => {
                self.installing = false;
                self.review = None;
                self.notice = match result {
                    Ok(receipt) => format!(
                        "{}{}",
                        receipt.message,
                        if receipt.restart_required {
                            " Restart Daedalus when ready; this session stays open."
                        } else {
                            ""
                        }
                    ),
                    Err(error) => format!(
                        "Update failed: {error}. Check the installed version before retrying."
                    ),
                };
            }
        }
    }
    fn consent(&mut self) -> Option<UpdateReview> {
        if !self.visible || self.installing {
            return None;
        }
        let review = self.review.take()?;
        self.installing = true;
        self.update_selected = false;
        self.notice = format!(
            "Installing {}… You can close this dialog and keep working.",
            review.version
        );
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
            KeyCode::Esc | KeyCode::Char('n' | 'N') => self.visible = false,
            KeyCode::Char('y' | 'Y') => self.install(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab if self.review.is_some() => {
                self.update_selected = !self.update_selected;
            }
            KeyCode::Enter if self.update_selected && self.review.is_some() => self.install(),
            KeyCode::Enter => self.visible = false,
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
                self.visible = false;
            } else if self.update_hit.contains(point) && self.review.is_some() {
                self.install();
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
            if self.review.is_some() || self.installing {
                let text = if self.installing {
                    "Updating… · Ctrl+U details".to_owned()
                } else {
                    format!(
                        "Update {} available · Ctrl+U",
                        self.review.as_ref().unwrap().version
                    )
                };
                let toast = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
                frame.render_widget(Paragraph::new(text).style(theme.fg(Tone::Accent)), toast);
            }
            return;
        }
        let width = area.width.min(76);
        let height = area.height.min(15);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .title(" Daedalus updates ")
            .borders(Borders::ALL)
            .border_set(theme.border())
            .border_style(theme.fg(Tone::Accent))
            .style(theme.surface());
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let detail = match &self.review {
            Some(review) => format!(
                "{}\n\nVersion: {} · {}\nDownload: {} bytes\nInstall: {}\n\nExisting binary is retained. Update only with your approval.",
                self.notice, review.version, review.platform, review.size, review.install_path
            ),
            None => self.notice.clone(),
        };
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(2),
        );
        frame.render_widget(
            Paragraph::new(detail)
                .wrap(Wrap { trim: true })
                .style(theme.fg(Tone::Text)),
            body,
        );
        if inner.height >= 2 {
            self.later_hit = Rect::new(inner.x, inner.bottom() - 1, inner.width.min(17), 1);
            let later = if self.review.is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
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
        ui.open();
        let review = ui.consent().unwrap();
        assert_eq!(review.approval_sha256, "c".repeat(64));
        assert!(ui.consent().is_none());
        ui.complete(Completion::Install(Ok(InstallReceipt {
            version: "1.1.0".into(),
            installed_path: "test".into(),
            previous_path: "old".into(),
            restart_required: true,
            activated: false,
            message: "Staged".into(),
        })));
        assert!(ui.notice.contains("Restart"));
        assert!(!ui.installing);
    }
    #[test]
    fn errors_and_current_release_are_visible_without_install_offer() {
        let mut ui = Controller::new(false);
        ui.complete(Completion::Check(Err("offline".into())));
        ui.open();
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
}
