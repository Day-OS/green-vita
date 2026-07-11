use egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

const CJK_FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansCJK-SC-Subset.otf");
const ARABIC_FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansArabic-Regular.ttf");
const DEJAVU_SYMBOLS_FONT: &[u8] =
    include_bytes!("../../../assets/fonts/DejaVuSansSymbols-Subset.ttf");
const SYMBOLS_FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSymbols-Subset.ttf");
const SYMBOLS_2_FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSymbols2-Subset.ttf");

pub(crate) fn configure(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu-symbols".to_owned(),
        Arc::new(FontData::from_static(DEJAVU_SYMBOLS_FONT)),
    );
    fonts.font_data.insert(
        "noto-symbols".to_owned(),
        Arc::new(FontData::from_static(SYMBOLS_FONT)),
    );
    fonts.font_data.insert(
        "noto-symbols-2".to_owned(),
        Arc::new(FontData::from_static(SYMBOLS_2_FONT)),
    );
    fonts.font_data.insert(
        "noto-arabic".to_owned(),
        Arc::new(FontData::from_static(ARABIC_FONT)),
    );
    fonts.font_data.insert(
        "noto-cjk".to_owned(),
        Arc::new(FontData::from_static(CJK_FONT)),
    );

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .get_mut(&family)
            .expect("default font family")
            .extend([
                "dejavu-symbols".to_owned(),
                "noto-symbols".to_owned(),
                "noto-symbols-2".to_owned(),
                "noto-arabic".to_owned(),
                "noto-cjk".to_owned(),
            ]);
    }

    ctx.set_fonts(fonts);
}
