//! The screen's colours, detected once at startup.
//!
//! A terminal's background decides which variant of each role is readable: the
//! light background gets the dark inks, the dark background the bright ones.
//! `COLORFGBG` is the only portable hint a terminal gives us about its
//! background; when it is missing or unreadable the dark variant is the safe
//! default (a dark background is the common case, and the reference does the same
//! choice).
//!
//! When colour is unavailable (`NO_COLOR`, `TERM=dumb`) every role becomes
//! [`Color::Reset`] and the styles carry meaning through bold, reverse, and dim
//! instead. [`Palette`] is the only place in the TUI that names a colour.

use ratatui::style::{Color, Modifier, Style};

/// The roles the screen paints with. Every field is a colour from the
/// `AdaptiveColor` pairs, except under [`Palette::plain`], where all of them are
/// [`Color::Reset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub ok: Color,
    pub bad: Color,
    /// True when the screen has no colour to give (NO_COLOR, TERM=dumb).
    pub plain: bool,
}

impl Palette {
    /// Detects once, at startup. `NO_COLOR` (any non-empty value) wins and
    /// yields a monochrome palette; otherwise the background decided by
    /// `COLORFGBG`'s last field picks the light or the dark variant.
    pub fn detect() -> Self {
        let no_color = std::env::var("NO_COLOR").ok();
        let term = std::env::var("TERM").ok();
        Self::from_env(no_color.as_deref(), term.as_deref(), is_light_background())
    }

    /// The palette decision without the process environment, so it can be
    /// exercised without mutating variables the test harness shares.
    fn from_env(no_color: Option<&str>, term: Option<&str>, light: bool) -> Self {
        let plain = no_color.is_some_and(|value| !value.is_empty()) || term == Some("dumb");
        if plain {
            Self::monochrome()
        } else {
            Self::for_background(light)
        }
    }

    /// Light and dark pairs: the dark ink for a light background, the
    /// bright ink for a dark one.
    fn for_background(light: bool) -> Self {
        if light {
            Self {
                accent: hex(0x4F46E5),
                text: hex(0x1F2328),
                muted: hex(0x8B8F98),
                faint: hex(0xC4C7CE),
                ok: hex(0x0F9D58),
                bad: hex(0xD93025),
                plain: false,
            }
        } else {
            Self {
                accent: hex(0xA5B4FC),
                text: hex(0xE6E6E6),
                muted: hex(0x7C8290),
                faint: hex(0x4A4F5A),
                ok: hex(0x7EE2A8),
                bad: hex(0xFF8A80),
                plain: false,
            }
        }
    }

    fn monochrome() -> Self {
        Self {
            accent: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            faint: Color::Reset,
            ok: Color::Reset,
            bad: Color::Reset,
            plain: true,
        }
    }

    pub fn text(&self) -> Style {
        self.role(self.text, Modifier::empty())
    }

    pub fn muted(&self) -> Style {
        self.role(self.muted, Modifier::DIM)
    }

    pub fn faint(&self) -> Style {
        self.role(self.faint, Modifier::DIM)
    }

    pub fn accent(&self) -> Style {
        self.role(self.accent, Modifier::BOLD)
    }

    pub fn ok(&self) -> Style {
        self.role(self.ok, Modifier::empty())
    }

    pub fn bad(&self) -> Style {
        // Loud in both modes: without hue, an error is still the one bold red
        // thing on the line.
        Style::new().fg(self.bad).add_modifier(Modifier::BOLD)
    }

    pub fn bold_text(&self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    /// Reverse video, so a selected row survives a monochrome palette.
    pub fn selected(&self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    /// Paints a role with the modifier that carries its meaning once hue is
    /// gone. With colour available the ink alone is enough.
    fn role(&self, color: Color, plain_modifier: Modifier) -> Style {
        let style = Style::new().fg(color);
        if self.plain {
            style.add_modifier(plain_modifier)
        } else {
            style
        }
    }
}

/// Whether the terminal's background is light, from `COLORFGBG`
/// (`"15;0"` = white on black → dark). Unknown means dark.
///
/// The last field is the background's ANSI index. Indexes 7–15 are the light
/// half of the palette (`0;7` is a white background); the rest, including a
/// missing or unparseable value, read as dark.
pub fn is_light_background() -> bool {
    light_from(std::env::var("COLORFGBG").ok().as_deref())
}

fn light_from(colorfgbg: Option<&str>) -> bool {
    let Some(value) = colorfgbg else {
        return false;
    };
    // `COLORFGBG` is `foreground;background`; a lone number says nothing about
    // the background, so it is treated as unknown.
    if !value.contains(';') {
        return false;
    }
    let last = value.rsplit(';').next().unwrap_or_default();
    last.trim()
        .parse::<u8>()
        .is_ok_and(|index| (7..=15).contains(&index))
}

const fn hex(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [Color; 6] = [Color::Reset; 6];

    #[test]
    fn colorfgbg_picks_the_background() {
        // The same decision `is_light_background` makes after reading the
        // variable; the environment itself is left untouched.
        assert!(!light_from(Some("15;0")), "white on black is a dark bg");
        assert!(light_from(Some("0;15")), "black on white is a light bg");
        assert!(light_from(Some("0;7")));
        assert!(!light_from(Some("1;4")));
        assert!(!light_from(None), "unknown means dark");
        assert!(!light_from(Some("garbage")), "unreadable means dark");
        assert!(!light_from(Some("")), "empty means dark");
        assert!(!light_from(Some("15")), "a lone field says nothing");
    }

    #[test]
    fn no_color_wins_and_goes_monochrome() {
        // `detect` is this decision plus two reads of the process environment.
        // Mutating that environment is unsafe (and this crate forbids unsafe),
        // so the decision itself is what the test drives here.
        let palette = Palette::from_env(Some("1"), None, true);
        assert!(palette.plain);
        assert_eq!(
            [
                palette.accent,
                palette.text,
                palette.muted,
                palette.faint,
                palette.ok,
                palette.bad,
            ],
            ROLES
        );
        // `TERM=dumb` is the other way to say "no colour", and an empty
        // `NO_COLOR` is the same as an unset one.
        assert!(Palette::from_env(None, Some("dumb"), false).plain);
        assert!(!Palette::from_env(Some(""), None, true).plain);
    }

    #[test]
    fn both_variants_are_defined() {
        let light = Palette::for_background(true);
        assert_eq!(light.accent, hex(0x4F46E5));
        assert_eq!(light.text, hex(0x1F2328));
        assert_eq!(light.muted, hex(0x8B8F98));
        assert_eq!(light.faint, hex(0xC4C7CE));
        assert_eq!(light.ok, hex(0x0F9D58));
        assert_eq!(light.bad, hex(0xD93025));
        assert!(!light.plain);

        let dark = Palette::for_background(false);
        assert_eq!(dark.accent, hex(0xA5B4FC));
        assert_eq!(dark.text, hex(0xE6E6E6));
        assert_eq!(dark.muted, hex(0x7C8290));
        assert_eq!(dark.faint, hex(0x4A4F5A));
        assert_eq!(dark.ok, hex(0x7EE2A8));
        assert_eq!(dark.bad, hex(0xFF8A80));
        assert!(!dark.plain);
    }

    #[test]
    fn plain_keeps_meaning_through_modifiers() {
        let plain = Palette::from_env(Some("1"), None, false);
        assert!(plain.selected().add_modifier.contains(Modifier::REVERSED));
        assert!(plain.muted().add_modifier.contains(Modifier::DIM));
        assert!(plain.faint().add_modifier.contains(Modifier::DIM));
        assert!(plain.accent().add_modifier.contains(Modifier::BOLD));
        assert!(plain.bad().add_modifier.contains(Modifier::BOLD));

        // Colour alone distinguishes a healthy call from an error when the
        // palette is not plain.
        let dark = Palette::for_background(false);
        assert_eq!(dark.ok().fg, Some(dark.ok));
        assert_eq!(dark.bad().fg, Some(dark.bad));
    }
}
