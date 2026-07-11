use egui::Color32;

/// Real Xbox brand green (`#107C10`).
const XBOX_GREEN: Color32 = Color32::from_rgb(0x10, 0x7c, 0x10);

#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_bright: Color32,
}

impl Theme {
    pub(crate) fn dark() -> Self {
        Self {
            background: Color32::from_rgb(0x1a, 0x1b, 0x1e),
            accent: XBOX_GREEN,
            text: Color32::from_rgb(0xd8, 0xd8, 0xdd),
            text_bright: Color32::from_rgb(0xf7, 0xf7, 0xf9),
        }
    }

    pub(crate) fn error() -> Self {
        Self {
            background: Color32::from_rgb(0x21, 0x0c, 0x0f),
            accent: Color32::from_rgb(0xf2, 0x51, 0x3f),
            text: Color32::from_rgb(0xea, 0xbc, 0xb7),
            text_bright: Color32::from_rgb(0xff, 0xea, 0xe0),
        }
    }
}
