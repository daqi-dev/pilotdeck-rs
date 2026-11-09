use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Paragraph, Wrap},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Shortcuts,
    Settings,
    About,
}

pub(super) enum Action {
    None,
    Close,
    CycleMode,
}

pub(super) struct Help {
    page: Page,
    cursor: usize,
    scroll: u16,
    pub error: Option<String>,
}
impl Default for Help {
    fn default() -> Self {
        Self {
            page: Page::Shortcuts,
            cursor: 0,
            scroll: 0,
            error: None,
        }
    }
}
impl Help {
    pub fn key(&mut self, key: KeyEvent, page_height: u16) -> Action {
        let page = match key.code {
            KeyCode::Char('1') => Some(Page::Shortcuts),
            KeyCode::Char('2') => {
                self.cursor = 0;
                Some(Page::Settings)
            }
            KeyCode::Char('3') => Some(Page::About),
            KeyCode::Right | KeyCode::Tab
                if self.page != Page::Settings && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                Some(match self.page {
                    Page::Shortcuts => Page::Settings,
                    _ => Page::Shortcuts,
                })
            }
            KeyCode::Left | KeyCode::BackTab if self.page != Page::Settings => {
                Some(match self.page {
                    Page::Shortcuts => Page::About,
                    _ => Page::Settings,
                })
            }
            KeyCode::Tab
                if self.page != Page::Settings && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                Some(match self.page {
                    Page::Shortcuts => Page::About,
                    _ => Page::Settings,
                })
            }
            KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
            KeyCode::Up if self.page == Page::Settings => {
                self.cursor = self.cursor.saturating_sub(1);
                None
            }
            KeyCode::Down if self.page == Page::Settings => {
                self.cursor = (self.cursor + 1).min(2);
                None
            }
            KeyCode::Enter | KeyCode::Char(' ')
                if self.page == Page::Settings && self.cursor == 0 =>
            {
                return Action::CycleMode;
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(page_height.max(1));
                None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(page_height.max(1));
                None
            }
            KeyCode::Up if self.page != Page::Settings => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            KeyCode::Down if self.page != Page::Settings => {
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            _ => None,
        };
        if let Some(page) = page {
            self.page = page;
            self.scroll = 0;
            self.error = None;
        }
        Action::None
    }
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        mode: &str,
        url: &str,
        session: &str,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let area = if self.error.is_some() {
            frame.render_widget(
                Paragraph::new("Save failed; mode unchanged"),
                Rect::new(area.x, area.y, area.width, 1),
            );
            Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height.saturating_sub(1),
            )
        } else {
            area
        };
        if area.height == 0 {
            return;
        }
        let mut preview = session.chars().take(256).collect::<String>();
        if session.chars().nth(256).is_some() {
            preview.push('…');
        }
        let body = match self.page {
            Page::Shortcuts => "Commands\n/new  Start a session\n/sessions  Open sessions\n/switch [N]  Select a session\n/mode [default|plan|bypassPermissions]  Set permission mode\n/permissions  Inspect and edit permission rules\n/view [N]  Open tool output\n/clear  Clear messages\n/help  Open help\n/exit  Exit\n\nKeys\nEnter  Submit input or expand focused tool\nTab / Shift-Tab  Focus tool output\nEsc  Clear focus or interrupt a turn\nCtrl-C  Abort or exit\nPage Up / Page Down  Scroll messages\n\nOutput viewer\nj/k or arrows  Scroll lines\nPage Up / Page Down  Scroll pages\ng/G  Start/end\nq/Esc  Close".to_owned(),
            Page::Settings => format!("Settings  (Up/Down select; Enter or Space changes mode)\n{} Mode: {}\n{} Connection: {}\n{} Model: PilotDeck\n\nSession: {}\n\nMode cycles through default, plan and bypass. Bypass skips permission checks.\n{}", if self.cursor==0 {">"}else{" "}, if mode=="bypassPermissions" {"bypass"} else {mode}, if self.cursor==1 {">"}else{" "}, url, if self.cursor==2 {">"}else{" "}, preview, self.error.as_deref().unwrap_or("")),
            Page::About => "PilotDeck\n\nA terminal interface for sessions and agent turns.\n\nConnects to a Gateway for conversations, tools and model execution.\n\nPermission prompts use y to allow, a to allow and remember, and n to deny.".to_owned(),
        };
        let header = format!(
            "Help: {}  |  1 Shortcuts  2 Settings  3 About  |  q/Esc close\n\n",
            match self.page {
                Page::Shortcuts => "Shortcuts",
                Page::Settings => "Settings",
                Page::About => "About",
            }
        );
        let content = format!("{header}{body}");
        let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
        let lines = paragraph.line_count(area.width);
        self.scroll = self.scroll.min(
            lines
                .saturating_sub(area.height as usize)
                .min(u16::MAX as usize) as u16,
        );
        frame.render_widget(paragraph.scroll((self.scroll, 0)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn save_error_stays_visible_while_scrolled() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(34, 6)).unwrap();
        let mut help = Help::default();
        help.key(key(KeyCode::Char('2')), 4);
        help.error = Some("disk error".into());
        help.scroll = 100;
        terminal
            .draw(|f| help.render(f, f.area(), "default", "remote", &"S".repeat(100_000)))
            .unwrap();
        let row: String = terminal.backend().buffer().content()[..34]
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("Save failed; mode unchanged"), "{row}");
    }
    #[test]
    fn settings_locks_tab_and_only_mode_cycles() {
        let mut help = Help::default();
        help.key(key(KeyCode::Tab), 5);
        assert_eq!(help.page, Page::Settings);
        help.key(key(KeyCode::Tab), 5);
        assert_eq!(help.page, Page::Settings);
        assert!(matches!(
            help.key(key(KeyCode::Enter), 5),
            Action::CycleMode
        ));
        help.key(key(KeyCode::Down), 5);
        assert!(matches!(help.key(key(KeyCode::Enter), 5), Action::None));
        help.key(key(KeyCode::Char('3')), 5);
        assert_eq!(help.page, Page::About);
        assert!(matches!(help.key(key(KeyCode::Esc), 5), Action::Close));
    }
}
