//! Implementation of font shaping for Arabic scripts
//!
//! Code herein follows the specification at:
//! <https://github.com/n8willis/opentype-shaping-documents/blob/master/opentype-shaping-arabic-general.md>

use crate::error::{ParseError, ShapingError};
use crate::gsub::{self, GlyphData, GlyphOrigin, GsubFeatureMask, RawGlyph};
use crate::layout::{GDEFTable, LayoutCache, LayoutTable, GSUB};
use crate::tag;

use std::convert::From;
use unicode_joining_type::{get_joining_type, JoiningType};

#[derive(Clone)]
struct ArabicData {
    joining_type: JoiningType,
    feature_tag: u32,
}

impl GlyphData for ArabicData {
    fn merge(data1: ArabicData, _data2: ArabicData) -> ArabicData {
        // TODO hold off for future Unicode normalisation changes
        data1
    }
}

// Arabic glyphs are represented as `RawGlyph` structs with `ArabicData` for its `extra_data`.
type ArabicGlyph = RawGlyph<ArabicData>;

impl ArabicGlyph {
    fn is_transparent(&self) -> bool {
        self.extra_data.joining_type == JoiningType::Transparent || self.multi_subst_dup
    }

    fn is_left_joining(&self) -> bool {
        self.extra_data.joining_type == JoiningType::LeftJoining
            || self.extra_data.joining_type == JoiningType::DualJoining
            || self.extra_data.joining_type == JoiningType::JoinCausing
    }

    fn is_right_joining(&self) -> bool {
        self.extra_data.joining_type == JoiningType::RightJoining
            || self.extra_data.joining_type == JoiningType::DualJoining
            || self.extra_data.joining_type == JoiningType::JoinCausing
    }

    fn feature_tag(&self) -> u32 {
        self.extra_data.feature_tag
    }

    fn set_feature_tag(&mut self, feature_tag: u32) {
        self.extra_data.feature_tag = feature_tag
    }
}

impl From<&RawGlyph<()>> for ArabicGlyph {
    fn from(raw_glyph: &RawGlyph<()>) -> ArabicGlyph {
        // Since there's no `Char` to work out the `ArabicGlyph`s joining type when the glyph's
        // `glyph_origin` is `GlyphOrigin::Direct`, we fallback to `JoiningType::NonJoining` as
        // the safest approach
        let joining_type = match raw_glyph.glyph_origin {
            GlyphOrigin::Char(c) => get_joining_type(c),
            GlyphOrigin::Direct => JoiningType::NonJoining,
        };

        ArabicGlyph {
            unicodes: raw_glyph.unicodes.clone(),
            glyph_index: raw_glyph.glyph_index,
            liga_component_pos: raw_glyph.liga_component_pos,
            glyph_origin: raw_glyph.glyph_origin,
            small_caps: raw_glyph.small_caps,
            multi_subst_dup: raw_glyph.multi_subst_dup,
            is_vert_alt: raw_glyph.is_vert_alt,
            fake_bold: raw_glyph.fake_bold,
            fake_italic: raw_glyph.fake_italic,
            variation: raw_glyph.variation,
            extra_data: ArabicData {
                joining_type,
                // For convenience, we loosely follow the spec (`2. Computing letter joining
                // states`) here by initialising all `ArabicGlyph`s to `tag::ISOL`
                feature_tag: tag::ISOL,
            },
        }
    }
}

impl From<&ArabicGlyph> for RawGlyph<()> {
    fn from(arabic_glyph: &ArabicGlyph) -> RawGlyph<()> {
        RawGlyph {
            unicodes: arabic_glyph.unicodes.clone(),
            glyph_index: arabic_glyph.glyph_index,
            liga_component_pos: arabic_glyph.liga_component_pos,
            glyph_origin: arabic_glyph.glyph_origin,
            small_caps: arabic_glyph.small_caps,
            multi_subst_dup: arabic_glyph.multi_subst_dup,
            is_vert_alt: arabic_glyph.is_vert_alt,
            fake_bold: arabic_glyph.fake_bold,
            variation: arabic_glyph.variation,
            fake_italic: arabic_glyph.fake_italic,
            extra_data: (),
        }
    }
}

pub fn gsub_apply_arabic(
    gsub_cache: &LayoutCache<GSUB>,
    gsub_table: &LayoutTable<GSUB>,
    gdef_table: Option<&GDEFTable>,
    script_tag: u32,
    lang_tag: Option<u32>,
    raw_glyphs: &mut Vec<RawGlyph<()>>,
) -> Result<(), ShapingError> {
    match gsub_table.find_script(script_tag)? {
        Some(s) => {
            if s.find_langsys_or_default(lang_tag)?.is_none() {
                return Ok(());
            }
        }
        None => return Ok(()),
    }

    let arabic_glyphs = &mut raw_glyphs.iter().map(ArabicGlyph::from).collect();

    // 1. Compound character composition and decomposition

    apply_lookups(
        GsubFeatureMask::CCMP,
        gsub_cache,
        gsub_table,
        gdef_table,
        script_tag,
        lang_tag,
        arabic_glyphs,
        |_, _| true,
    )?;

    // 2. Computing letter joining states

    {
        let mut previous_i = arabic_glyphs
            .iter()
            .position(|g| !g.is_transparent())
            .unwrap_or(0);

        for i in (previous_i + 1)..arabic_glyphs.len() {
            if arabic_glyphs[i].is_transparent() {
                continue;
            }

            if arabic_glyphs[previous_i].is_left_joining() && arabic_glyphs[i].is_right_joining() {
                arabic_glyphs[i].set_feature_tag(tag::FINA);

                match arabic_glyphs[previous_i].feature_tag() {
                    tag::ISOL => arabic_glyphs[previous_i].set_feature_tag(tag::INIT),
                    tag::FINA => arabic_glyphs[previous_i].set_feature_tag(tag::MEDI),
                    _ => {}
                }
            }

            previous_i = i;
        }
    }

    // 3. Applying the stch feature
    //
    // TODO hold off for future generalised solution (including the Syriac Abbreviation Mark)

    // 4. Applying the language-form substitution features from GSUB

    const LANGUAGE_FEATURES: &'static [(GsubFeatureMask, bool)] = &[
        (GsubFeatureMask::LOCL, true),
        (GsubFeatureMask::ISOL, false),
        (GsubFeatureMask::FINA, false),
        (GsubFeatureMask::MEDI, false),
        (GsubFeatureMask::INIT, false),
        (GsubFeatureMask::RLIG, true),
        (GsubFeatureMask::RCLT, true),
        (GsubFeatureMask::CALT, true),
    ];

    for &(feature_mask, is_global) in LANGUAGE_FEATURES {
        apply_lookups(
            feature_mask,
            gsub_cache,
            gsub_table,
            gdef_table,
            script_tag,
            lang_tag,
            arabic_glyphs,
            |g, feature_tag| is_global || g.feature_tag() == feature_tag,
        )?;
    }

    // 5. Applying the typographic-form substitution features from GSUB
    //
    // Note that we skip `GSUB`'s `DLIG` and `CSWH` features as results would differ from other
    // Arabic shapers

    const TYPOGRAPHIC_FEATURES: &'static [GsubFeatureMask] =
        &[GsubFeatureMask::LIGA, GsubFeatureMask::MSET];

    for &feature_mask in TYPOGRAPHIC_FEATURES {
        apply_lookups(
            feature_mask,
            gsub_cache,
            gsub_table,
            gdef_table,
            script_tag,
            lang_tag,
            arabic_glyphs,
            |_, _| true,
        )?;
    }

    // 6. Mark reordering
    //
    // This is currently not implemented as results would then differ from other Arabic shapers

    *raw_glyphs = arabic_glyphs.iter().map(RawGlyph::from).collect();

    Ok(())
}

fn apply_lookups(
    feature_mask: GsubFeatureMask,
    gsub_cache: &LayoutCache<GSUB>,
    gsub_table: &LayoutTable<GSUB>,
    gdef_table: Option<&GDEFTable>,
    script_tag: u32,
    lang_tag: Option<u32>,
    arabic_glyphs: &mut Vec<RawGlyph<ArabicData>>,
    pred: impl Fn(&RawGlyph<ArabicData>, u32) -> bool + Copy,
) -> Result<(), ParseError> {
    let index = gsub::get_lookups_cache_index(gsub_cache, script_tag, lang_tag, feature_mask)?;
    let lookups = &gsub_cache.cached_lookups.borrow()[index];

    for &(lookup_index, feature_tag) in lookups {
        gsub::gsub_apply_lookup(
            gsub_cache,
            gsub_table,
            gdef_table,
            lookup_index,
            feature_tag,
            None,
            arabic_glyphs,
            0,
            arabic_glyphs.len(),
            |g| pred(g, feature_tag),
        )?;
    }

    Ok(())
}
