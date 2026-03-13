//! TrueType bytecode hinting (grid-fitting) interpreter.
//!
//! This module implements the TrueType instruction set interpreter that
//! executes bytecode embedded in font programs (`fpgm`, `prep`) and
//! individual glyph instructions to snap glyph outlines to the pixel grid.
//!
//! # Usage
//!
//! ```ignore
//! use allsorts::hinting::{HintInstance, HintedGlyph};
//! use allsorts::tables::FontTableProvider;
//!
//! // 1. Create a hint instance from font tables (runs fpgm)
//! let instance = HintInstance::new(&font_table_provider)?;
//!
//! // 2. Set size (runs prep, scales CVT)
//! instance.set_size(16.0, 96)?;
//!
//! // 3. Hint individual glyphs
//! let hinted = instance.hint_simple_glyph(&glyph, &outline)?;
//! // Use hinted.points for rasterization
//! ```

pub mod f26dot6;
pub mod graphics_state;
pub mod interpreter;

pub use f26dot6::{F26Dot6, F2Dot14};
pub use interpreter::{HintError, Interpreter, Point, Zone};

use crate::binary::read::ReadScope;
use crate::tables::{CvtTable, FontTableProvider, MaxpTable};
use crate::tag;

/// High-level hinting state for a font.
///
/// Created once per font. Runs `fpgm` at creation time to populate
/// function definitions. Call `set_size` to prepare for a specific ppem.
pub struct HintInstance {
    pub interpreter: Interpreter,
    fpgm_executed: bool,
    prep_bytecode: Vec<u8>,
    cvt_funits: Vec<i16>,
}

impl HintInstance {
    /// Create a new hinting instance from font table data.
    ///
    /// Parses `maxp`, `cvt`, `fpgm`, and `prep` tables, then executes `fpgm`
    /// to populate function definitions.
    ///
    /// Returns `None` if the font has no TrueType hinting data.
    pub fn new(provider: &dyn FontTableProvider) -> Result<Option<Self>, HintError> {
        // Read maxp table
        let maxp = match provider.table_data(tag::MAXP) {
            Ok(Some(data)) => {
                let scope = ReadScope::new(&data);
                match scope.read::<MaxpTable>() {
                    Ok(maxp) => maxp,
                    Err(_) => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        let maxp_v1 = match &maxp.version1_sub_table {
            Some(v1) => v1,
            None => return Ok(None), // CFF font, no TrueType hinting
        };

        // Read CVT table (optional)
        let cvt_funits: Vec<i16> = match provider.table_data(tag::CVT) {
            Ok(Some(data)) => {
                let len = data.len() as u32;
                let scope = ReadScope::new(&data);
                match scope.ctxt().read_dep::<CvtTable<'_>>(len) {
                    Ok(cvt) => {
                        let mut values = Vec::with_capacity(cvt.values.len());
                        for i in 0..cvt.values.len() {
                            if let Some(v) = cvt.values.get_item(i) {
                                values.push(v);
                            }
                        }
                        values
                    }
                    Err(_) => Vec::new(),
                }
            }
            _ => Vec::new(),
        };

        // Read fpgm bytecode (optional)
        let fpgm_bytecode: Vec<u8> = match provider.table_data(tag::FPGM) {
            Ok(Some(data)) => data.into_owned(),
            _ => Vec::new(),
        };

        // Read prep bytecode (optional)
        let prep_bytecode: Vec<u8> = match provider.table_data(tag::PREP) {
            Ok(Some(data)) => data.into_owned(),
            _ => Vec::new(),
        };

        // Create interpreter
        let mut interpreter = Interpreter::new(
            maxp_v1.max_stack_elements,
            maxp_v1.max_storage,
            maxp_v1.max_function_defs,
            maxp_v1.max_instruction_defs,
            maxp_v1.max_twilight_points,
            0, // units_per_em set later
        );

        // Read head table for units_per_em
        if let Ok(Some(head_data)) = provider.table_data(tag::HEAD) {
            if head_data.len() >= 20 {
                let upem = u16::from_be_bytes([head_data[18], head_data[19]]);
                interpreter.units_per_em = upem;
            }
        }

        // Execute fpgm to populate function definitions
        let mut fpgm_executed = false;
        if !fpgm_bytecode.is_empty() {
            match interpreter.execute_fpgm(&fpgm_bytecode) {
                Ok(()) => fpgm_executed = true,
                Err(_e) => {
                    // fpgm execution failed; continue without hinting functions
                    // Some fonts have buggy fpgm programs
                }
            }
        } else {
            fpgm_executed = true; // No fpgm = nothing to execute
        }

        Ok(Some(HintInstance {
            interpreter,
            fpgm_executed,
            prep_bytecode,
            cvt_funits,
        }))
    }

    /// Prepare the interpreter for a specific point size and DPI.
    ///
    /// Scales CVT values and executes the `prep` program.
    pub fn set_size(&mut self, point_size: f64, dpi: u16) -> Result<(), HintError> {
        let ppem = ((point_size * dpi as f64) / 72.0).round() as u16;
        self.set_ppem(ppem, point_size)
    }

    /// Prepare the interpreter for a specific ppem value.
    pub fn set_ppem(&mut self, ppem: u16, point_size: f64) -> Result<(), HintError> {
        // Scale CVT from FUnits to F26Dot6
        self.interpreter.ppem = ppem;
        self.interpreter.scale =
            f26dot6::compute_scale(ppem, self.interpreter.units_per_em);
        self.interpreter.scale_cvt(&self.cvt_funits);

        // Execute prep program
        if !self.prep_bytecode.is_empty() && self.fpgm_executed {
            // Ignore prep errors (some fonts have buggy prep programs)
            let _ = self
                .interpreter
                .execute_prep(&self.prep_bytecode, ppem, point_size);
        }

        Ok(())
    }

    /// Hint a simple glyph outline.
    ///
    /// Takes points already scaled to F26Dot6 pixel coordinates, the on-curve
    /// flags, contour end indices, and the per-glyph instruction bytecode.
    ///
    /// Returns the hinted point positions as (x, y) pairs in F26Dot6.
    pub fn hint_glyph(
        &mut self,
        points_f26dot6: &[(i32, i32)],
        on_curve: &[bool],
        contour_ends: &[u16],
        instructions: &[u8],
    ) -> Result<Vec<(i32, i32)>, HintError> {
        if instructions.is_empty() || !self.fpgm_executed {
            // No instructions: return points unchanged
            return Ok(points_f26dot6.to_vec());
        }

        let points: Vec<Point> = points_f26dot6
            .iter()
            .map(|&(x, y)| Point { x, y })
            .collect();

        self.interpreter
            .hint_glyph(&points, on_curve, contour_ends, instructions)?;

        // Extract hinted positions
        let result: Vec<(i32, i32)> = self.interpreter.zones[1]
            .current
            .iter()
            .take(points.len())
            .map(|p| (p.x, p.y))
            .collect();

        Ok(result)
    }
}
