use std::fmt;

use super::f26dot6::{compute_scale, F2Dot14, F26Dot6};
use super::graphics_state::{GraphicsState, RoundState};

// ── Error type ───────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintError {
    StackOverflow,
    StackUnderflow,
    InvalidOpcode(u8),
    UndefinedFunction(u32),
    CallStackOverflow,
    InvalidPointIndex(u32),
    InvalidCvtIndex(u32),
    InvalidStorageIndex(u32),
    InvalidZone(u32),
    DivideByZero,
    UnexpectedEndOfBytecode,
    InvalidJump,
    ExceededMaxInstructions,
    FontNotReady,
}

impl fmt::Display for HintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HintError::StackOverflow => write!(f, "hinting: stack overflow"),
            HintError::StackUnderflow => write!(f, "hinting: stack underflow"),
            HintError::InvalidOpcode(op) => write!(f, "hinting: invalid opcode 0x{:02X}", op),
            HintError::UndefinedFunction(id) => {
                write!(f, "hinting: undefined function {}", id)
            }
            HintError::CallStackOverflow => write!(f, "hinting: call stack overflow"),
            HintError::InvalidPointIndex(i) => {
                write!(f, "hinting: invalid point index {}", i)
            }
            HintError::InvalidCvtIndex(i) => write!(f, "hinting: invalid CVT index {}", i),
            HintError::InvalidStorageIndex(i) => {
                write!(f, "hinting: invalid storage index {}", i)
            }
            HintError::InvalidZone(z) => write!(f, "hinting: invalid zone {}", z),
            HintError::DivideByZero => write!(f, "hinting: divide by zero"),
            HintError::UnexpectedEndOfBytecode => {
                write!(f, "hinting: unexpected end of bytecode")
            }
            HintError::InvalidJump => write!(f, "hinting: invalid jump target"),
            HintError::ExceededMaxInstructions => {
                write!(f, "hinting: exceeded maximum instruction count")
            }
            HintError::FontNotReady => write!(f, "hinting: font not ready (fpgm not executed)"),
        }
    }
}

impl std::error::Error for HintError {}

// ── Point / Zone types ───────────────────────────────────────────────

#[derive(Copy, Clone, Debug, Default)]
pub struct Point {
    pub x: i32, // F26Dot6
    pub y: i32, // F26Dot6
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug, Default)]
    pub struct PointFlags: u8 {
        const TOUCHED_X = 0x01;
        const TOUCHED_Y = 0x02;
        const ON_CURVE  = 0x04;
    }
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub original: Vec<Point>,
    pub current: Vec<Point>,
    pub flags: Vec<PointFlags>,
    pub contour_ends: Vec<u16>,
}

impl Zone {
    pub fn new(n_points: usize) -> Self {
        Zone {
            original: vec![Point::default(); n_points],
            current: vec![Point::default(); n_points],
            flags: vec![PointFlags::empty(); n_points],
            contour_ends: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.current.len()
    }

    pub fn resize(&mut self, n: usize) {
        self.original.resize(n, Point::default());
        self.current.resize(n, Point::default());
        self.flags.resize(n, PointFlags::empty());
    }
}

// ── Function definition ──────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct FuncDef {
    pub bytecode: Vec<u8>,
}

// ── Maximum instruction count to prevent infinite loops ──────────────

const MAX_INSTRUCTIONS: u64 = 1_000_000;
const MAX_CALL_DEPTH: u32 = 64;

// ── Interpreter ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Interpreter {
    // Stack
    pub(crate) stack: Vec<i32>,
    max_stack: usize,

    // CVT (F26Dot6 values stored as i32)
    pub(crate) cvt: Vec<i32>,

    // Storage area
    pub(crate) storage: Vec<i32>,

    // Function/instruction definitions
    pub(crate) fdefs: Vec<Option<FuncDef>>,
    pub(crate) idefs: Vec<Option<FuncDef>>,

    // Graphics state
    pub(crate) gs: GraphicsState,
    pub(crate) default_gs: GraphicsState,

    // Zones: 0 = twilight, 1 = glyph
    pub(crate) zones: [Zone; 2],

    // Font metrics
    pub(crate) ppem: u16,
    pub(crate) point_size: i32, // F26Dot6
    pub(crate) units_per_em: u16,
    pub(crate) scale: i64, // 16.16 fixed-point

    // Execution state
    instruction_count: u64,
    call_depth: u32,
}

impl Interpreter {
    /// Create a new interpreter from maxp table values.
    pub fn new(
        max_stack_elements: u16,
        max_storage: u16,
        max_function_defs: u16,
        max_instruction_defs: u16,
        max_twilight_points: u16,
        units_per_em: u16,
    ) -> Self {
        Interpreter {
            stack: Vec::with_capacity(max_stack_elements as usize),
            max_stack: max_stack_elements as usize,
            cvt: Vec::new(),
            storage: vec![0i32; max_storage as usize],
            fdefs: vec![None; max_function_defs as usize],
            idefs: vec![None; max_instruction_defs as usize],
            gs: GraphicsState::default(),
            default_gs: GraphicsState::default(),
            zones: [
                Zone::new(max_twilight_points as usize), // twilight
                Zone::new(0),                            // glyph (resized per glyph)
            ],
            ppem: 0,
            point_size: 0,
            units_per_em,
            scale: 0,
            instruction_count: 0,
            call_depth: 0,
        }
    }

    /// Execute the font program (`fpgm`) to populate function definitions.
    pub fn execute_fpgm(&mut self, fpgm: &[u8]) -> Result<(), HintError> {
        self.stack.clear();
        self.gs = GraphicsState::default();
        self.instruction_count = 0;
        self.call_depth = 0;
        self.execute(fpgm)
    }

    /// Set the ppem size and execute the prep program.
    pub fn execute_prep(&mut self, prep: &[u8], ppem: u16, point_size: f64) -> Result<(), HintError> {
        self.ppem = ppem;
        self.point_size = F26Dot6::from_f64(point_size).to_bits();
        self.scale = compute_scale(ppem, self.units_per_em);

        self.stack.clear();
        self.gs = GraphicsState::default();
        self.instruction_count = 0;
        self.call_depth = 0;
        self.execute(prep)?;

        // Save the modified graphics state as the default for glyph programs
        self.default_gs = self.gs.clone();
        Ok(())
    }

    /// Scale CVT values from FUnits to F26Dot6 pixels.
    pub fn scale_cvt(&mut self, cvt_funits: &[i16]) {
        self.cvt.clear();
        self.cvt.reserve(cvt_funits.len());
        for &funit in cvt_funits {
            self.cvt
                .push(F26Dot6::from_funits(funit as i32, self.scale).to_bits());
        }
    }

    /// Hint a glyph outline by executing its bytecode instructions.
    ///
    /// `points` are pre-scaled to F26Dot6 coordinates.
    /// `on_curve` flags indicate whether each point is on-curve.
    /// `contour_ends` gives the index of the last point in each contour.
    /// `instructions` is the per-glyph bytecode from the glyf table.
    ///
    /// After execution, the hinted point positions can be read from
    /// `self.zones[1].current`.
    pub fn hint_glyph(
        &mut self,
        points: &[Point],
        on_curve: &[bool],
        contour_ends: &[u16],
        instructions: &[u8],
    ) -> Result<(), HintError> {
        // Set up the glyph zone
        let n = points.len();
        let zone = &mut self.zones[1];
        zone.resize(n);
        for i in 0..n {
            zone.original[i] = points[i];
            zone.current[i] = points[i];
            let mut flags = PointFlags::empty();
            if on_curve.get(i).copied().unwrap_or(false) {
                flags |= PointFlags::ON_CURVE;
            }
            zone.flags[i] = flags;
        }
        zone.contour_ends = contour_ends.to_vec();

        // Reset graphics state to defaults (set by prep)
        self.gs = self.default_gs.clone();
        self.gs.rp0 = 0;
        self.gs.rp1 = 0;
        self.gs.rp2 = 0;
        self.gs.loop_value = 1;
        self.gs.zp0 = 1;
        self.gs.zp1 = 1;
        self.gs.zp2 = 1;

        self.stack.clear();
        self.instruction_count = 0;
        self.call_depth = 0;

        self.execute(instructions)
    }

    // ── Core execution loop ──────────────────────────────────────────

    fn execute(&mut self, bytecode: &[u8]) -> Result<(), HintError> {
        let mut ip: usize = 0;
        while ip < bytecode.len() {
            self.instruction_count += 1;
            if self.instruction_count > MAX_INSTRUCTIONS {
                return Err(HintError::ExceededMaxInstructions);
            }

            let opcode = bytecode[ip];
            ip += 1;

            self.dispatch(opcode, bytecode, &mut ip)?;
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        opcode: u8,
        bytecode: &[u8],
        ip: &mut usize,
    ) -> Result<(), HintError> {
        match opcode {
            // ── Vector setting ───────────────────────────────────
            0x00 => {
                // SVTCA[y] - set both vectors to y-axis
                self.gs.projection_vector = (F2Dot14::ZERO, F2Dot14::ONE);
                self.gs.freedom_vector = (F2Dot14::ZERO, F2Dot14::ONE);
                self.gs.dual_projection_vector = (F2Dot14::ZERO, F2Dot14::ONE);
            }
            0x01 => {
                // SVTCA[x] - set both vectors to x-axis
                self.gs.projection_vector = (F2Dot14::ONE, F2Dot14::ZERO);
                self.gs.freedom_vector = (F2Dot14::ONE, F2Dot14::ZERO);
                self.gs.dual_projection_vector = (F2Dot14::ONE, F2Dot14::ZERO);
            }
            0x02 => {
                // SPVTCA[y] - set projection vector to y-axis
                self.gs.projection_vector = (F2Dot14::ZERO, F2Dot14::ONE);
                self.gs.dual_projection_vector = (F2Dot14::ZERO, F2Dot14::ONE);
            }
            0x03 => {
                // SPVTCA[x] - set projection vector to x-axis
                self.gs.projection_vector = (F2Dot14::ONE, F2Dot14::ZERO);
                self.gs.dual_projection_vector = (F2Dot14::ONE, F2Dot14::ZERO);
            }
            0x04 => {
                // SFVTCA[y] - set freedom vector to y-axis
                self.gs.freedom_vector = (F2Dot14::ZERO, F2Dot14::ONE);
            }
            0x05 => {
                // SFVTCA[x] - set freedom vector to x-axis
                self.gs.freedom_vector = (F2Dot14::ONE, F2Dot14::ZERO);
            }
            0x06..=0x07 => {
                // SPVTL[a] - set projection vector to line
                let a = opcode & 1;
                self.op_spvtl(a != 0)?;
            }
            0x08..=0x09 => {
                // SFVTL[a] - set freedom vector to line
                let a = opcode & 1;
                self.op_sfvtl(a != 0)?;
            }
            0x0A => {
                // SPVFS - set projection vector from stack
                let y = self.pop()? as i16 as i32;
                let x = self.pop()? as i16 as i32;
                self.gs.projection_vector = (F2Dot14::from_bits(x), F2Dot14::from_bits(y));
                self.gs.dual_projection_vector = self.gs.projection_vector;
            }
            0x0B => {
                // SFVFS - set freedom vector from stack
                let y = self.pop()? as i16 as i32;
                let x = self.pop()? as i16 as i32;
                self.gs.freedom_vector = (F2Dot14::from_bits(x), F2Dot14::from_bits(y));
            }
            0x0C => {
                // GPV - get projection vector
                self.push(self.gs.projection_vector.0.to_bits())?;
                self.push(self.gs.projection_vector.1.to_bits())?;
            }
            0x0D => {
                // GFV - get freedom vector
                self.push(self.gs.freedom_vector.0.to_bits())?;
                self.push(self.gs.freedom_vector.1.to_bits())?;
            }
            0x0E => {
                // SFVTPV - set freedom vector to projection vector
                self.gs.freedom_vector = self.gs.projection_vector;
            }
            0x0F => {
                // ISECT - move point to intersection
                self.op_isect()?;
            }

            // ── Reference point / zone setting ───────────────────
            0x10 => {
                // SRP0
                self.gs.rp0 = self.pop()? as u32;
            }
            0x11 => {
                // SRP1
                self.gs.rp1 = self.pop()? as u32;
            }
            0x12 => {
                // SRP2
                self.gs.rp2 = self.pop()? as u32;
            }
            0x13 => {
                // SZP0
                let zone = self.pop()? as u32;
                if zone > 1 {
                    return Err(HintError::InvalidZone(zone));
                }
                self.gs.zp0 = zone;
            }
            0x14 => {
                // SZP1
                let zone = self.pop()? as u32;
                if zone > 1 {
                    return Err(HintError::InvalidZone(zone));
                }
                self.gs.zp1 = zone;
            }
            0x15 => {
                // SZP2
                let zone = self.pop()? as u32;
                if zone > 1 {
                    return Err(HintError::InvalidZone(zone));
                }
                self.gs.zp2 = zone;
            }
            0x16 => {
                // SZPS - set all zone pointers
                let zone = self.pop()? as u32;
                if zone > 1 {
                    return Err(HintError::InvalidZone(zone));
                }
                self.gs.zp0 = zone;
                self.gs.zp1 = zone;
                self.gs.zp2 = zone;
            }
            0x17 => {
                // SLOOP
                let n = self.pop()?;
                self.gs.loop_value = n.max(1) as u32;
            }

            // ── Rounding mode ────────────────────────────────────
            0x18 => {
                // RTG - round to grid
                self.gs.round_state = RoundState::Grid;
            }
            0x19 => {
                // RTHG - round to half grid
                self.gs.round_state = RoundState::HalfGrid;
            }

            // ── Distances ────────────────────────────────────────
            0x1A => {
                // SMD - set minimum distance
                let d = self.pop()?;
                self.gs.minimum_distance = F26Dot6::from_bits(d);
            }

            // ── Control flow ─────────────────────────────────────
            0x1B => {
                // ELSE - skip to EIF
                self.skip_else(bytecode, ip)?;
            }
            0x1C => {
                // JMPR - jump relative
                let offset = self.pop()?;
                let new_ip = (*ip as i64) + (offset as i64) - 1;
                if new_ip < 0 || new_ip > bytecode.len() as i64 {
                    return Err(HintError::InvalidJump);
                }
                *ip = new_ip as usize;
            }
            0x1D => {
                // SCVTCI - set CVT cut-in
                let v = self.pop()?;
                self.gs.control_value_cut_in = F26Dot6::from_bits(v);
            }
            0x1E => {
                // SSWCI - set single width cut-in
                let v = self.pop()?;
                self.gs.single_width_cut_in = F26Dot6::from_bits(v);
            }
            0x1F => {
                // SSW - set single width value (FUnits -> F26Dot6)
                let v = self.pop()?;
                self.gs.single_width_value =
                    F26Dot6::from_funits(v, self.scale);
            }

            // ── Stack manipulation ───────────────────────────────
            0x20 => {
                // DUP
                let v = self.peek()?;
                self.push(v)?;
            }
            0x21 => {
                // POP
                self.pop()?;
            }
            0x22 => {
                // CLEAR
                self.stack.clear();
            }
            0x23 => {
                // SWAP
                let len = self.stack.len();
                if len < 2 {
                    return Err(HintError::StackUnderflow);
                }
                self.stack.swap(len - 1, len - 2);
            }
            0x24 => {
                // DEPTH
                let d = self.stack.len() as i32;
                self.push(d)?;
            }
            0x25 => {
                // CINDEX - copy indexed element
                let idx = self.pop()? as usize;
                let len = self.stack.len();
                if idx == 0 || idx > len {
                    return Err(HintError::StackUnderflow);
                }
                let v = self.stack[len - idx];
                self.push(v)?;
            }
            0x26 => {
                // MINDEX - move indexed element to top
                let idx = self.pop()? as usize;
                let len = self.stack.len();
                if idx == 0 || idx > len {
                    return Err(HintError::StackUnderflow);
                }
                let pos = len - idx;
                let v = self.stack.remove(pos);
                self.stack.push(v);
            }

            // ── Point alignment ──────────────────────────────────
            0x27 => {
                // ALIGNPTS
                self.op_alignpts()?;
            }

            0x29 => {
                // UTP - untouch point
                let p = self.pop()? as u32;
                let zp0 = self.gs.zp0 as usize;
                if let Some(flags) = self.zones[zp0].flags.get_mut(p as usize) {
                    // Clear touched flags based on freedom vector
                    if self.gs.freedom_vector.0.to_bits() != 0 {
                        flags.remove(PointFlags::TOUCHED_X);
                    }
                    if self.gs.freedom_vector.1.to_bits() != 0 {
                        flags.remove(PointFlags::TOUCHED_Y);
                    }
                }
            }

            // ── Function calls ───────────────────────────────────
            0x2A => {
                // LOOPCALL
                let fn_id = self.pop()? as u32;
                let count = self.pop()? as u32;
                if self.call_depth >= MAX_CALL_DEPTH {
                    return Err(HintError::CallStackOverflow);
                }
                let func = self
                    .fdefs
                    .get(fn_id as usize)
                    .and_then(|f| f.as_ref())
                    .ok_or(HintError::UndefinedFunction(fn_id))?
                    .bytecode
                    .clone();
                for _ in 0..count {
                    self.call_depth += 1;
                    self.execute(&func)?;
                    self.call_depth -= 1;
                }
            }
            0x2B => {
                // CALL
                let fn_id = self.pop()? as u32;
                if self.call_depth >= MAX_CALL_DEPTH {
                    return Err(HintError::CallStackOverflow);
                }
                let func = self
                    .fdefs
                    .get(fn_id as usize)
                    .and_then(|f| f.as_ref())
                    .ok_or(HintError::UndefinedFunction(fn_id))?
                    .bytecode
                    .clone();
                self.call_depth += 1;
                self.execute(&func)?;
                self.call_depth -= 1;
            }
            0x2C => {
                // FDEF - function definition
                let fn_id = self.pop()? as u32;
                let start = *ip;
                // Scan forward to find ENDF (0x2D), handling nested FDEF/ENDF
                let end = self.find_endf(bytecode, ip)?;
                let func_bytecode = bytecode[start..end].to_vec();
                if (fn_id as usize) < self.fdefs.len() {
                    self.fdefs[fn_id as usize] = Some(FuncDef {
                        bytecode: func_bytecode,
                    });
                }
                // ip now points past ENDF
            }
            0x2D => {
                // ENDF - end of function
                // In normal execution flow (inside CALL), this returns from execute()
                return Ok(());
            }

            // ── Point movement ───────────────────────────────────
            0x2E..=0x2F => {
                // MDAP[r] - move direct absolute point
                let round = opcode & 1 != 0;
                self.op_mdap(round)?;
            }
            0x30..=0x31 => {
                // IUP[a] - interpolate untouched points
                let axis = opcode & 1; // 0 = y, 1 = x
                self.op_iup(axis)?;
            }
            0x32..=0x33 => {
                // SHP[a] - shift point
                let use_rp1 = opcode & 1 == 0;
                self.op_shp(use_rp1)?;
            }
            0x34..=0x35 => {
                // SHC[a] - shift contour
                let use_rp1 = opcode & 1 == 0;
                self.op_shc(use_rp1)?;
            }
            0x36..=0x37 => {
                // SHZ[a] - shift zone
                let use_rp1 = opcode & 1 == 0;
                self.op_shz(use_rp1)?;
            }
            0x38 => {
                // SHPIX - shift point by pixel amount
                self.op_shpix()?;
            }
            0x39 => {
                // IP - interpolate point
                self.op_ip()?;
            }
            0x3A..=0x3B => {
                // MSIRP[a] - move stack indirect relative point
                let set_rp0 = opcode & 1 != 0;
                self.op_msirp(set_rp0)?;
            }
            0x3C => {
                // ALIGNRP - align relative point
                self.op_alignrp()?;
            }
            0x3D => {
                // RTDG - round to double grid
                self.gs.round_state = RoundState::DoubleGrid;
            }
            0x3E..=0x3F => {
                // MIAP[r] - move indirect absolute point
                let round = opcode & 1 != 0;
                self.op_miap(round)?;
            }

            // ── Push instructions ────────────────────────────────
            0x40 => {
                // NPUSHB - push n bytes
                if *ip >= bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                let n = bytecode[*ip] as usize;
                *ip += 1;
                if *ip + n > bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                for i in 0..n {
                    self.push(bytecode[*ip + i] as i32)?;
                }
                *ip += n;
            }
            0x41 => {
                // NPUSHW - push n words
                if *ip >= bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                let n = bytecode[*ip] as usize;
                *ip += 1;
                if *ip + n * 2 > bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                for i in 0..n {
                    let hi = bytecode[*ip + i * 2] as i16;
                    let lo = bytecode[*ip + i * 2 + 1] as u8;
                    let val = ((hi as i32) << 8) | (lo as i32);
                    self.push(val)?;
                }
                *ip += n * 2;
            }

            // ── Storage / CVT ────────────────────────────────────
            0x42 => {
                // WS - write storage
                let val = self.pop()?;
                let idx = self.pop()? as u32;
                let i = idx as usize;
                if i >= self.storage.len() {
                    return Err(HintError::InvalidStorageIndex(idx));
                }
                self.storage[i] = val;
            }
            0x43 => {
                // RS - read storage
                let idx = self.pop()? as u32;
                let i = idx as usize;
                if i >= self.storage.len() {
                    return Err(HintError::InvalidStorageIndex(idx));
                }
                self.push(self.storage[i])?;
            }
            0x44 => {
                // WCVTP - write CVT in pixel units (F26Dot6)
                let val = self.pop()?;
                let idx = self.pop()? as u32;
                let i = idx as usize;
                if i >= self.cvt.len() {
                    // Extend CVT if needed
                    self.cvt.resize(i + 1, 0);
                }
                self.cvt[i] = val;
            }
            0x45 => {
                // RCVT - read CVT
                let idx = self.pop()? as u32;
                let val = self.read_cvt(idx)?;
                self.push(val)?;
            }
            0x46..=0x47 => {
                // GC[a] - get coordinate (0=current, 1=original)
                let use_original = opcode & 1 != 0;
                self.op_gc(use_original)?;
            }
            0x48 => {
                // SCFS - set coordinate from stack
                self.op_scfs()?;
            }
            0x49..=0x4A => {
                // MD[a] - measure distance (0=current, 1=original)
                let use_original = opcode & 1 != 0;
                self.op_md(use_original)?;
            }
            0x4B => {
                // MPPEM - measure pixels per em
                self.push(self.ppem as i32)?;
            }
            0x4C => {
                // MPS - measure point size (F26Dot6)
                self.push(self.point_size)?;
            }

            // ── Boolean / flip ───────────────────────────────────
            0x4D => {
                // FLIPON
                self.gs.auto_flip = true;
            }
            0x4E => {
                // FLIPOFF
                self.gs.auto_flip = false;
            }
            0x4F => {
                // DEBUG - no-op
            }

            // ── Comparison ───────────────────────────────────────
            0x50 => {
                // LT
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a < b { 1 } else { 0 })?;
            }
            0x51 => {
                // LTEQ
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a <= b { 1 } else { 0 })?;
            }
            0x52 => {
                // GT
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a > b { 1 } else { 0 })?;
            }
            0x53 => {
                // GTEQ
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a >= b { 1 } else { 0 })?;
            }
            0x54 => {
                // EQ
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a == b { 1 } else { 0 })?;
            }
            0x55 => {
                // NEQ
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != b { 1 } else { 0 })?;
            }
            0x56 => {
                // ODD
                let v = self.pop()?;
                let rounded = self.gs.round(F26Dot6::from_bits(v));
                self.push(if (rounded.to_i32() & 1) != 0 { 1 } else { 0 })?;
            }
            0x57 => {
                // EVEN
                let v = self.pop()?;
                let rounded = self.gs.round(F26Dot6::from_bits(v));
                self.push(if (rounded.to_i32() & 1) == 0 { 1 } else { 0 })?;
            }

            // ── IF / ELSE / EIF ──────────────────────────────────
            0x58 => {
                // IF
                let cond = self.pop()?;
                if cond == 0 {
                    self.skip_to_else_or_eif(bytecode, ip)?;
                }
            }
            0x59 => {
                // EIF - end if (no-op when reached normally)
            }

            // ── Logic ────────────────────────────────────────────
            0x5A => {
                // AND
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != 0 && b != 0 { 1 } else { 0 })?;
            }
            0x5B => {
                // OR
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(if a != 0 || b != 0 { 1 } else { 0 })?;
            }
            0x5C => {
                // NOT
                let v = self.pop()?;
                self.push(if v == 0 { 1 } else { 0 })?;
            }

            // ── Delta instructions ───────────────────────────────
            0x5D => self.op_deltap(1, bytecode, ip)?, // DELTAP1
            0x5E => {
                // SDB - set delta base
                self.gs.delta_base = self.pop()? as u16;
            }
            0x5F => {
                // SDS - set delta shift
                self.gs.delta_shift = self.pop()? as u16;
            }

            // ── Arithmetic ───────────────────────────────────────
            0x60 => {
                // ADD
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(a.wrapping_add(b))?;
            }
            0x61 => {
                // SUB
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(a.wrapping_sub(b))?;
            }
            0x62 => {
                // DIV
                let b = self.pop()?;
                if b == 0 {
                    return Err(HintError::DivideByZero);
                }
                let a = self.pop()?;
                // F26Dot6 division: (a << 6) / b
                let result = ((a as i64) << 6) / (b as i64);
                self.push(result as i32)?;
            }
            0x63 => {
                // MUL
                let b = self.pop()?;
                let a = self.pop()?;
                // F26Dot6 multiplication: (a * b) >> 6
                let result = ((a as i64) * (b as i64)) >> 6;
                self.push(result as i32)?;
            }
            0x64 => {
                // ABS
                let v = self.pop()?;
                self.push(v.abs())?;
            }
            0x65 => {
                // NEG
                let v = self.pop()?;
                self.push(-v)?;
            }
            0x66 => {
                // FLOOR
                let v = self.pop()?;
                self.push(v & !63)?;
            }
            0x67 => {
                // CEILING
                let v = self.pop()?;
                self.push((v + 63) & !63)?;
            }

            // ── ROUND / NROUND ───────────────────────────────────
            0x68..=0x6B => {
                // ROUND[ab] - round value
                let v = self.pop()?;
                let result = self.gs.round(F26Dot6::from_bits(v));
                self.push(result.to_bits())?;
            }
            0x6C..=0x6F => {
                // NROUND[ab] - no-round (pass through)
                // Value stays on stack as-is
            }

            // ── More CVT / Delta ─────────────────────────────────
            0x70 => {
                // WCVTF - write CVT in FUnits
                let val = self.pop()?; // value in FUnits
                let idx = self.pop()? as u32;
                let scaled = F26Dot6::from_funits(val, self.scale).to_bits();
                let i = idx as usize;
                if i >= self.cvt.len() {
                    self.cvt.resize(i + 1, 0);
                }
                self.cvt[i] = scaled;
            }
            0x71 => self.op_deltap(2, bytecode, ip)?, // DELTAP2
            0x72 => self.op_deltap(3, bytecode, ip)?, // DELTAP3
            0x73 => self.op_deltac(1)?,               // DELTAC1
            0x74 => self.op_deltac(2)?,               // DELTAC2
            0x75 => self.op_deltac(3)?,               // DELTAC3

            // ── Super rounding ───────────────────────────────────
            0x76 => {
                // SROUND
                let n = self.pop()? as u32;
                self.gs.set_super_round(n, false);
                self.gs.round_state = RoundState::Super;
            }
            0x77 => {
                // S45ROUND
                let n = self.pop()? as u32;
                self.gs.set_super_round(n, true);
                self.gs.round_state = RoundState::Super45;
            }

            // ── Conditional jumps ────────────────────────────────
            0x78 => {
                // JROT - jump relative on true
                let cond = self.pop()?;
                let offset = self.pop()?;
                if cond != 0 {
                    let new_ip = (*ip as i64) + (offset as i64) - 2;
                    if new_ip < 0 || new_ip > bytecode.len() as i64 {
                        return Err(HintError::InvalidJump);
                    }
                    *ip = new_ip as usize;
                }
            }
            0x79 => {
                // JROF - jump relative on false
                let cond = self.pop()?;
                let offset = self.pop()?;
                if cond == 0 {
                    let new_ip = (*ip as i64) + (offset as i64) - 2;
                    if new_ip < 0 || new_ip > bytecode.len() as i64 {
                        return Err(HintError::InvalidJump);
                    }
                    *ip = new_ip as usize;
                }
            }

            0x7A => {
                // ROFF - round off
                self.gs.round_state = RoundState::Off;
            }

            0x7C => {
                // RUTG - round up to grid
                self.gs.round_state = RoundState::UpToGrid;
            }
            0x7D => {
                // RDTG - round down to grid
                self.gs.round_state = RoundState::DownToGrid;
            }
            0x7E => {
                // SANGW (obsolete) - no-op
                self.pop()?;
            }
            0x7F => {
                // AA (obsolete) - no-op
                self.pop()?;
            }

            // ── Flip instructions ────────────────────────────────
            0x80 => {
                // FLIPPT
                self.op_flippt()?;
            }
            0x81 => {
                // FLIPRGON
                let hi = self.pop()? as usize;
                let lo = self.pop()? as usize;
                for i in lo..=hi {
                    if let Some(flags) = self.zones[1].flags.get_mut(i) {
                        flags.insert(PointFlags::ON_CURVE);
                    }
                }
            }
            0x82 => {
                // FLIPRGOFF
                let hi = self.pop()? as usize;
                let lo = self.pop()? as usize;
                for i in lo..=hi {
                    if let Some(flags) = self.zones[1].flags.get_mut(i) {
                        flags.remove(PointFlags::ON_CURVE);
                    }
                }
            }

            0x85 => {
                // SCANCTRL
                self.gs.scan_control = self.pop()? as u32;
            }
            0x86..=0x87 => {
                // SDPVTL[a] - set dual projection vector to line
                let a = opcode & 1;
                self.op_sdpvtl(a != 0)?;
            }
            0x88 => {
                // GETINFO
                self.op_getinfo()?;
            }
            0x89 => {
                // IDEF - instruction definition
                let instr_id = self.pop()? as u32;
                let start = *ip;
                let end = self.find_endf(bytecode, ip)?;
                let func_bytecode = bytecode[start..end].to_vec();
                if (instr_id as usize) < self.idefs.len() {
                    self.idefs[instr_id as usize] = Some(FuncDef {
                        bytecode: func_bytecode,
                    });
                }
            }
            0x8A => {
                // ROLL - roll top 3 stack elements
                let len = self.stack.len();
                if len < 3 {
                    return Err(HintError::StackUnderflow);
                }
                let a = self.stack[len - 1];
                let b = self.stack[len - 2];
                let c = self.stack[len - 3];
                self.stack[len - 1] = b;
                self.stack[len - 2] = c;
                self.stack[len - 3] = a;
            }
            0x8B => {
                // MAX
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(a.max(b))?;
            }
            0x8C => {
                // MIN
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(a.min(b))?;
            }
            0x8D => {
                // SCANTYPE
                self.gs.scan_type = self.pop()?;
            }
            0x8E => {
                // INSTCTRL
                let s = self.pop()? as u32;
                let v = self.pop()? as u32;
                if s >= 1 && s <= 3 {
                    let mask = 1u8 << (s - 1);
                    if v != 0 {
                        self.gs.instruct_control |= mask;
                    } else {
                        self.gs.instruct_control &= !mask;
                    }
                }
            }

            // ── PUSHB / PUSHW ────────────────────────────────────
            0xB0..=0xB7 => {
                // PUSHB[n] - push n+1 bytes
                let count = (opcode - 0xB0 + 1) as usize;
                if *ip + count > bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                for i in 0..count {
                    self.push(bytecode[*ip + i] as i32)?;
                }
                *ip += count;
            }
            0xB8..=0xBF => {
                // PUSHW[n] - push n+1 words (signed 16-bit)
                let count = (opcode - 0xB8 + 1) as usize;
                if *ip + count * 2 > bytecode.len() {
                    return Err(HintError::UnexpectedEndOfBytecode);
                }
                for i in 0..count {
                    let hi = bytecode[*ip + i * 2] as i8;
                    let lo = bytecode[*ip + i * 2 + 1];
                    let val = ((hi as i32) << 8) | (lo as i32);
                    self.push(val)?;
                }
                *ip += count * 2;
            }

            // ── MDRP ─────────────────────────────────────────────
            0xC0..=0xDF => {
                // MDRP[abcde] - move direct relative point
                self.op_mdrp(opcode)?;
            }

            // ── MIRP ─────────────────────────────────────────────
            0xE0..=0xFF => {
                // MIRP[abcde] - move indirect relative point
                self.op_mirp(opcode)?;
            }

            // Unused / reserved opcodes
            _ => {
                // Check IDEFs for this opcode
                if let Some(Some(idef)) = self.idefs.get(opcode as usize) {
                    let func = idef.bytecode.clone();
                    self.call_depth += 1;
                    self.execute(&func)?;
                    self.call_depth -= 1;
                }
                // Otherwise silently ignore (compatibility)
            }
        }
        Ok(())
    }

    // ── Stack helpers ────────────────────────────────────────────────

    fn push(&mut self, val: i32) -> Result<(), HintError> {
        if self.stack.len() >= self.max_stack {
            return Err(HintError::StackOverflow);
        }
        self.stack.push(val);
        Ok(())
    }

    fn pop(&mut self) -> Result<i32, HintError> {
        self.stack.pop().ok_or(HintError::StackUnderflow)
    }

    fn peek(&self) -> Result<i32, HintError> {
        self.stack.last().copied().ok_or(HintError::StackUnderflow)
    }

    fn read_cvt(&self, idx: u32) -> Result<i32, HintError> {
        self.cvt
            .get(idx as usize)
            .copied()
            .ok_or(HintError::InvalidCvtIndex(idx))
    }

    // ── Control flow helpers ─────────────────────────────────────────

    /// Skip bytecode until matching ELSE or EIF for an IF whose condition was false.
    fn skip_to_else_or_eif(
        &self,
        bytecode: &[u8],
        ip: &mut usize,
    ) -> Result<(), HintError> {
        let mut depth = 1u32;
        while *ip < bytecode.len() {
            let op = bytecode[*ip];
            *ip += 1;
            match op {
                0x58 => depth += 1, // nested IF
                0x59 => {
                    // EIF
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                0x1B => {
                    // ELSE
                    if depth == 1 {
                        return Ok(());
                    }
                }
                // Skip inline data for push instructions
                0x40 => {
                    // NPUSHB
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n;
                }
                0x41 => {
                    // NPUSHW
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n * 2;
                }
                0xB0..=0xB7 => *ip += (op - 0xB0 + 1) as usize,
                0xB8..=0xBF => *ip += ((op - 0xB8 + 1) * 2) as usize,
                _ => {}
            }
        }
        Err(HintError::UnexpectedEndOfBytecode)
    }

    /// Skip from ELSE to matching EIF.
    fn skip_else(
        &self,
        bytecode: &[u8],
        ip: &mut usize,
    ) -> Result<(), HintError> {
        let mut depth = 1u32;
        while *ip < bytecode.len() {
            let op = bytecode[*ip];
            *ip += 1;
            match op {
                0x58 => depth += 1, // nested IF
                0x59 => {
                    // EIF
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                // Skip inline data
                0x40 => {
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n;
                }
                0x41 => {
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n * 2;
                }
                0xB0..=0xB7 => *ip += (op - 0xB0 + 1) as usize,
                0xB8..=0xBF => *ip += ((op - 0xB8 + 1) * 2) as usize,
                _ => {}
            }
        }
        Err(HintError::UnexpectedEndOfBytecode)
    }

    /// Find matching ENDF for a FDEF/IDEF, advancing ip past it.
    fn find_endf(&self, bytecode: &[u8], ip: &mut usize) -> Result<usize, HintError> {
        let mut depth = 1u32;
        while *ip < bytecode.len() {
            let op = bytecode[*ip];
            *ip += 1;
            match op {
                0x2C | 0x89 => depth += 1, // nested FDEF or IDEF
                0x2D => {
                    // ENDF
                    depth -= 1;
                    if depth == 0 {
                        return Ok(*ip - 1); // position of ENDF
                    }
                }
                // Skip inline data
                0x40 => {
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n;
                }
                0x41 => {
                    if *ip >= bytecode.len() {
                        return Err(HintError::UnexpectedEndOfBytecode);
                    }
                    let n = bytecode[*ip] as usize;
                    *ip += 1 + n * 2;
                }
                0xB0..=0xB7 => *ip += (op - 0xB0 + 1) as usize,
                0xB8..=0xBF => *ip += ((op - 0xB8 + 1) * 2) as usize,
                _ => {}
            }
        }
        Err(HintError::UnexpectedEndOfBytecode)
    }

    // ── Projection / freedom vector helpers ──────────────────────────

    /// Project a point onto the projection vector, returning F26Dot6 distance.
    fn project(&self, p: Point) -> i32 {
        let (px, py) = self.gs.projection_vector;
        ((p.x as i64 * px.to_bits() as i64 + p.y as i64 * py.to_bits() as i64 + 0x2000) >> 14)
            as i32
    }

    /// Project using the dual projection vector (for measuring original distances).
    fn dual_project(&self, p: Point) -> i32 {
        let (px, py) = self.gs.dual_projection_vector;
        ((p.x as i64 * px.to_bits() as i64 + p.y as i64 * py.to_bits() as i64 + 0x2000) >> 14)
            as i32
    }

    /// Move a point along the freedom vector by a given F26Dot6 distance.
    fn move_point(&mut self, zone: usize, point: usize, distance: i32) {
        if point >= self.zones[zone].current.len() {
            return;
        }

        let (fx, fy) = self.gs.freedom_vector;
        let (px, py) = self.gs.projection_vector;

        // Compute the dot product of freedom and projection vectors
        let dot = (fx.to_bits() as i64 * px.to_bits() as i64
            + fy.to_bits() as i64 * py.to_bits() as i64
            + 0x2000)
            >> 14;

        if dot == 0 {
            return;
        }

        // displacement = distance * freedom_vector / (freedom_vector · projection_vector)
        let dx = ((distance as i64 * fx.to_bits() as i64 + (dot >> 1)) / dot) as i32;
        let dy = ((distance as i64 * fy.to_bits() as i64 + (dot >> 1)) / dot) as i32;

        self.zones[zone].current[point].x += dx;
        self.zones[zone].current[point].y += dy;

        // Set touched flags
        if fx.to_bits() != 0 {
            self.zones[zone].flags[point].insert(PointFlags::TOUCHED_X);
        }
        if fy.to_bits() != 0 {
            self.zones[zone].flags[point].insert(PointFlags::TOUCHED_Y);
        }
    }

    // ── Point / zone access helpers ──────────────────────────────────

    fn get_point(&self, zone: usize, index: u32) -> Result<Point, HintError> {
        self.zones
            .get(zone)
            .and_then(|z| z.current.get(index as usize))
            .copied()
            .ok_or(HintError::InvalidPointIndex(index))
    }

    fn get_original_point(&self, zone: usize, index: u32) -> Result<Point, HintError> {
        self.zones
            .get(zone)
            .and_then(|z| z.original.get(index as usize))
            .copied()
            .ok_or(HintError::InvalidPointIndex(index))
    }

    // ── Vector-from-line instructions ────────────────────────────────

    fn op_spvtl(&mut self, perpendicular: bool) -> Result<(), HintError> {
        let p2_idx = self.pop()? as u32;
        let p1_idx = self.pop()? as u32;
        let p1 = self.get_point(self.gs.zp1 as usize, p1_idx)?;
        let p2 = self.get_point(self.gs.zp2 as usize, p2_idx)?;
        let v = self.compute_vector_from_line(p1, p2, perpendicular);
        self.gs.projection_vector = v;
        self.gs.dual_projection_vector = v;
        Ok(())
    }

    fn op_sfvtl(&mut self, perpendicular: bool) -> Result<(), HintError> {
        let p2_idx = self.pop()? as u32;
        let p1_idx = self.pop()? as u32;
        let p1 = self.get_point(self.gs.zp1 as usize, p1_idx)?;
        let p2 = self.get_point(self.gs.zp2 as usize, p2_idx)?;
        self.gs.freedom_vector = self.compute_vector_from_line(p1, p2, perpendicular);
        Ok(())
    }

    fn op_sdpvtl(&mut self, perpendicular: bool) -> Result<(), HintError> {
        let p2_idx = self.pop()? as u32;
        let p1_idx = self.pop()? as u32;

        // Use current points for projection vector
        let p1 = self.get_point(self.gs.zp1 as usize, p1_idx)?;
        let p2 = self.get_point(self.gs.zp2 as usize, p2_idx)?;
        self.gs.projection_vector = self.compute_vector_from_line(p1, p2, perpendicular);

        // Use original points for dual projection vector
        let op1 = self.get_original_point(self.gs.zp1 as usize, p1_idx)?;
        let op2 = self.get_original_point(self.gs.zp2 as usize, p2_idx)?;
        self.gs.dual_projection_vector = self.compute_vector_from_line(op1, op2, perpendicular);

        Ok(())
    }

    fn compute_vector_from_line(
        &self,
        p1: Point,
        p2: Point,
        perpendicular: bool,
    ) -> (F2Dot14, F2Dot14) {
        let dx = (p2.x - p1.x) as f64;
        let dy = (p2.y - p1.y) as f64;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1.0e-10 {
            return (F2Dot14::ONE, F2Dot14::ZERO);
        }
        let (nx, ny) = if perpendicular {
            (-dy / len, dx / len)
        } else {
            (dx / len, dy / len)
        };
        (F2Dot14::from_f64(nx), F2Dot14::from_f64(ny))
    }

    // ── Point movement instructions ──────────────────────────────────

    fn op_mdap(&mut self, round: bool) -> Result<(), HintError> {
        let p = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;

        let point = self.get_point(zp0, p)?;
        let cur_dist = self.project(point);

        let distance = if round {
            let rounded = self.gs.round(F26Dot6::from_bits(cur_dist));
            rounded.to_bits() - cur_dist
        } else {
            0
        };

        self.move_point(zp0, p as usize, distance);
        self.gs.rp0 = p;
        self.gs.rp1 = p;
        Ok(())
    }

    fn op_miap(&mut self, round: bool) -> Result<(), HintError> {
        let cvt_idx = self.pop()? as u32;
        let p = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;

        let cvt_val = self.read_cvt(cvt_idx)?;
        let point = self.get_point(zp0, p)?;
        let cur_dist = self.project(point);

        let distance = if round {
            let diff = (cvt_val - cur_dist).abs();
            let target = if diff <= self.gs.control_value_cut_in.to_bits() {
                cvt_val
            } else {
                cur_dist
            };
            let rounded = self.gs.round(F26Dot6::from_bits(target));
            rounded.to_bits() - cur_dist
        } else {
            cvt_val - cur_dist
        };

        self.move_point(zp0, p as usize, distance);
        self.gs.rp0 = p;
        self.gs.rp1 = p;
        Ok(())
    }

    fn op_mdrp(&mut self, opcode: u8) -> Result<(), HintError> {
        let p = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;

        // Flags from opcode bits: 0xC0 + [set_rp0, respect_min_dist, round, _, _]
        let set_rp0 = (opcode >> 4) & 1 != 0;
        let respect_min_dist = (opcode >> 3) & 1 != 0;
        let do_round = (opcode >> 2) & 1 != 0;

        // Measure original distance between rp0 and point
        let rp0_orig = self.get_original_point(zp0, self.gs.rp0)?;
        let p_orig = self.get_original_point(zp1, p)?;
        let mut dist = self.dual_project(Point {
            x: p_orig.x - rp0_orig.x,
            y: p_orig.y - rp0_orig.y,
        });

        // Apply single width
        let swv = self.gs.single_width_value.to_bits();
        if swv != 0 {
            let swci = self.gs.single_width_cut_in.to_bits();
            if (dist - swv).abs() < swci {
                dist = if dist >= 0 { swv } else { -swv };
            }
        }

        if do_round {
            dist = self.gs.round(F26Dot6::from_bits(dist)).to_bits();
        }

        if respect_min_dist {
            let min_dist = self.gs.minimum_distance.to_bits();
            if dist >= 0 {
                if dist < min_dist {
                    dist = min_dist;
                }
            } else if dist > -min_dist {
                dist = -min_dist;
            }
        }

        // Current position of reference point
        let rp0_cur = self.get_point(zp0, self.gs.rp0)?;
        let p_cur = self.get_point(zp1, p)?;
        let cur_dist = self.project(Point {
            x: p_cur.x - rp0_cur.x,
            y: p_cur.y - rp0_cur.y,
        });

        self.move_point(zp1, p as usize, dist - cur_dist);

        self.gs.rp1 = self.gs.rp0;
        self.gs.rp2 = p;
        if set_rp0 {
            self.gs.rp0 = p;
        }
        Ok(())
    }

    fn op_mirp(&mut self, opcode: u8) -> Result<(), HintError> {
        let cvt_idx = self.pop()? as u32;
        let p = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;

        let set_rp0 = (opcode >> 4) & 1 != 0;
        let respect_min_dist = (opcode >> 3) & 1 != 0;
        let do_round = (opcode >> 2) & 1 != 0;

        let cvt_val = if cvt_idx < self.cvt.len() as u32 {
            self.cvt[cvt_idx as usize]
        } else {
            0
        };

        // Measure original distance
        let rp0_orig = self.get_original_point(zp0, self.gs.rp0)?;
        let p_orig = self.get_original_point(zp1, p)?;
        let orig_dist = self.dual_project(Point {
            x: p_orig.x - rp0_orig.x,
            y: p_orig.y - rp0_orig.y,
        });

        let mut dist = cvt_val;

        // Auto flip
        if self.gs.auto_flip {
            if (orig_dist >= 0) != (dist >= 0) {
                dist = -dist;
            }
        }

        // Apply single width
        let swv = self.gs.single_width_value.to_bits();
        if swv != 0 {
            let swci = self.gs.single_width_cut_in.to_bits();
            if (dist - swv).abs() < swci {
                dist = if dist >= 0 { swv } else { -swv };
            }
        }

        // CVT cut-in: if actual distance is too far from CVT value, use actual
        let cvt_ci = self.gs.control_value_cut_in.to_bits();
        if do_round && (dist - orig_dist).abs() > cvt_ci {
            dist = orig_dist;
        }

        if do_round {
            dist = self.gs.round(F26Dot6::from_bits(dist)).to_bits();
        }

        if respect_min_dist {
            let min_dist = self.gs.minimum_distance.to_bits();
            if dist >= 0 {
                if dist < min_dist {
                    dist = min_dist;
                }
            } else if dist > -min_dist {
                dist = -min_dist;
            }
        }

        // Move point
        let rp0_cur = self.get_point(zp0, self.gs.rp0)?;
        let p_cur = self.get_point(zp1, p)?;
        let cur_dist = self.project(Point {
            x: p_cur.x - rp0_cur.x,
            y: p_cur.y - rp0_cur.y,
        });

        self.move_point(zp1, p as usize, dist - cur_dist);

        self.gs.rp1 = self.gs.rp0;
        self.gs.rp2 = p;
        if set_rp0 {
            self.gs.rp0 = p;
        }
        Ok(())
    }

    fn op_msirp(&mut self, set_rp0: bool) -> Result<(), HintError> {
        let dist = self.pop()?; // F26Dot6
        let p = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;

        let rp0_cur = self.get_point(zp0, self.gs.rp0)?;
        let p_cur = self.get_point(zp1, p)?;
        let cur_dist = self.project(Point {
            x: p_cur.x - rp0_cur.x,
            y: p_cur.y - rp0_cur.y,
        });

        self.move_point(zp1, p as usize, dist - cur_dist);

        self.gs.rp1 = self.gs.rp0;
        self.gs.rp2 = p;
        if set_rp0 {
            self.gs.rp0 = p;
        }
        Ok(())
    }

    fn op_alignrp(&mut self) -> Result<(), HintError> {
        let loop_count = self.gs.loop_value;
        self.gs.loop_value = 1;

        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;
        let rp0 = self.gs.rp0;

        for _ in 0..loop_count {
            let p = self.pop()? as u32;
            let rp0_cur = self.get_point(zp0, rp0)?;
            let p_cur = self.get_point(zp1, p)?;
            let cur_dist = self.project(Point {
                x: p_cur.x - rp0_cur.x,
                y: p_cur.y - rp0_cur.y,
            });
            self.move_point(zp1, p as usize, -cur_dist);
        }
        Ok(())
    }

    fn op_alignpts(&mut self) -> Result<(), HintError> {
        let p2 = self.pop()? as u32;
        let p1 = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;

        let p1_cur = self.get_point(zp1, p1)?;
        let p2_cur = self.get_point(zp0, p2)?;
        let dist = self.project(Point {
            x: p2_cur.x - p1_cur.x,
            y: p2_cur.y - p1_cur.y,
        });

        let half = dist / 2;
        self.move_point(zp1, p1 as usize, half);
        self.move_point(zp0, p2 as usize, -(dist - half));
        Ok(())
    }

    fn op_shp(&mut self, use_rp1: bool) -> Result<(), HintError> {
        let loop_count = self.gs.loop_value;
        self.gs.loop_value = 1;

        let (rp, rp_zone) = if use_rp1 {
            (self.gs.rp1, self.gs.zp0 as usize)
        } else {
            (self.gs.rp2, self.gs.zp1 as usize)
        };
        let zp2 = self.gs.zp2 as usize;

        // Compute displacement: difference between current and original of rp
        let rp_cur = self.get_point(rp_zone, rp)?;
        let rp_orig = self.get_original_point(rp_zone, rp)?;
        let displacement = self.project(Point {
            x: rp_cur.x - rp_orig.x,
            y: rp_cur.y - rp_orig.y,
        });

        for _ in 0..loop_count {
            let p = self.pop()? as u32;
            self.move_point(zp2, p as usize, displacement);
        }
        Ok(())
    }

    fn op_shc(&mut self, use_rp1: bool) -> Result<(), HintError> {
        let contour = self.pop()? as usize;

        let (rp, rp_zone) = if use_rp1 {
            (self.gs.rp1, self.gs.zp0 as usize)
        } else {
            (self.gs.rp2, self.gs.zp1 as usize)
        };
        let zp2 = self.gs.zp2 as usize;

        let rp_cur = self.get_point(rp_zone, rp)?;
        let rp_orig = self.get_original_point(rp_zone, rp)?;
        let displacement = self.project(Point {
            x: rp_cur.x - rp_orig.x,
            y: rp_cur.y - rp_orig.y,
        });

        // Get contour point range
        let start = if contour == 0 {
            0
        } else {
            self.zones[zp2]
                .contour_ends
                .get(contour - 1)
                .map(|&e| e as usize + 1)
                .unwrap_or(0)
        };
        let end = self.zones[zp2]
            .contour_ends
            .get(contour)
            .map(|&e| e as usize + 1)
            .unwrap_or(self.zones[zp2].len());

        for i in start..end {
            if i as u32 != rp {
                self.move_point(zp2, i, displacement);
            }
        }
        Ok(())
    }

    fn op_shz(&mut self, use_rp1: bool) -> Result<(), HintError> {
        let zone_idx = self.pop()? as u32;
        if zone_idx > 1 {
            return Err(HintError::InvalidZone(zone_idx));
        }

        let (rp, rp_zone) = if use_rp1 {
            (self.gs.rp1, self.gs.zp0 as usize)
        } else {
            (self.gs.rp2, self.gs.zp1 as usize)
        };

        let rp_cur = self.get_point(rp_zone, rp)?;
        let rp_orig = self.get_original_point(rp_zone, rp)?;
        let displacement = self.project(Point {
            x: rp_cur.x - rp_orig.x,
            y: rp_cur.y - rp_orig.y,
        });

        let z = zone_idx as usize;
        let n = self.zones[z].len();
        for i in 0..n {
            self.move_point(z, i, displacement);
        }
        Ok(())
    }

    fn op_shpix(&mut self) -> Result<(), HintError> {
        let dist = self.pop()?; // F26Dot6 pixels
        let loop_count = self.gs.loop_value;
        self.gs.loop_value = 1;

        let zp2 = self.gs.zp2 as usize;
        let (fx, fy) = self.gs.freedom_vector;

        for _ in 0..loop_count {
            let p = self.pop()? as u32;
            let i = p as usize;
            if i < self.zones[zp2].current.len() {
                // Move directly along freedom vector (no projection)
                self.zones[zp2].current[i].x +=
                    ((dist as i64 * fx.to_bits() as i64 + 0x2000) >> 14) as i32;
                self.zones[zp2].current[i].y +=
                    ((dist as i64 * fy.to_bits() as i64 + 0x2000) >> 14) as i32;

                if fx.to_bits() != 0 {
                    self.zones[zp2].flags[i].insert(PointFlags::TOUCHED_X);
                }
                if fy.to_bits() != 0 {
                    self.zones[zp2].flags[i].insert(PointFlags::TOUCHED_Y);
                }
            }
        }
        Ok(())
    }

    fn op_ip(&mut self) -> Result<(), HintError> {
        let loop_count = self.gs.loop_value;
        self.gs.loop_value = 1;

        let zp0 = self.gs.zp0 as usize;
        let zp1 = self.gs.zp1 as usize;
        let zp2 = self.gs.zp2 as usize;

        // Get reference points (original and current)
        let rp1_orig = self.get_original_point(zp0, self.gs.rp1)?;
        let rp2_orig = self.get_original_point(zp1, self.gs.rp2)?;
        let rp1_cur = self.get_point(zp0, self.gs.rp1)?;
        let rp2_cur = self.get_point(zp1, self.gs.rp2)?;

        let orig_range = self.dual_project(Point {
            x: rp2_orig.x - rp1_orig.x,
            y: rp2_orig.y - rp1_orig.y,
        });
        let cur_range = self.project(Point {
            x: rp2_cur.x - rp1_cur.x,
            y: rp2_cur.y - rp1_cur.y,
        });

        for _ in 0..loop_count {
            let p = self.pop()? as u32;
            let p_orig = self.get_original_point(zp2, p)?;
            let p_cur = self.get_point(zp2, p)?;

            let orig_dist = self.dual_project(Point {
                x: p_orig.x - rp1_orig.x,
                y: p_orig.y - rp1_orig.y,
            });

            let new_dist = if orig_range != 0 {
                // Interpolate: new_dist = cur_range * orig_dist / orig_range
                ((cur_range as i64 * orig_dist as i64 + (orig_range as i64 / 2))
                    / orig_range as i64) as i32
            } else {
                orig_dist
            };

            let cur_dist = self.project(Point {
                x: p_cur.x - rp1_cur.x,
                y: p_cur.y - rp1_cur.y,
            });

            self.move_point(zp2, p as usize, new_dist - cur_dist);
        }
        Ok(())
    }

    fn op_iup(&mut self, axis: u8) -> Result<(), HintError> {
        let n_points = self.zones[1].len();
        if n_points == 0 {
            return Ok(());
        }

        let touched_flag = if axis == 1 {
            PointFlags::TOUCHED_X
        } else {
            PointFlags::TOUCHED_Y
        };

        // Collect all (contour_start, contour_end, touched_points) first
        // to avoid borrowing self.zones[1] while calling self.iup_interp.
        let mut work: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        let mut contour_start = 0usize;
        let contour_ends: Vec<u16> = self.zones[1].contour_ends.clone();

        for &contour_end_u16 in &contour_ends {
            let contour_end = contour_end_u16 as usize;
            if contour_end >= n_points {
                break;
            }

            let mut touched_points: Vec<usize> = Vec::new();
            for i in contour_start..=contour_end {
                if self.zones[1].flags[i].contains(touched_flag) {
                    touched_points.push(i);
                }
            }

            if !touched_points.is_empty() {
                work.push((contour_start, contour_end, touched_points));
            }
            contour_start = contour_end + 1;
        }

        // Now perform interpolation
        for (contour_start, contour_end, touched_points) in &work {
            let n_touched = touched_points.len();
            for t in 0..n_touched {
                let t1_idx = touched_points[t];
                let t2_idx = touched_points[(t + 1) % n_touched];
                self.iup_interp(*contour_start, *contour_end, t1_idx, t2_idx, axis);
            }
        }
        Ok(())
    }

    fn iup_interp(
        &mut self,
        contour_start: usize,
        contour_end: usize,
        t1_idx: usize,
        t2_idx: usize,
        axis: u8,
    ) {
        let touched_flag = if axis == 1 {
            PointFlags::TOUCHED_X
        } else {
            PointFlags::TOUCHED_Y
        };

        // Walk from t1 to t2 (wrapping around contour), interpolating untouched points
        let contour_len = contour_end - contour_start + 1;

        let get_coord = |p: &Point| -> i32 {
            if axis == 1 { p.x } else { p.y }
        };

        let t1_orig = get_coord(&self.zones[1].original[t1_idx]);
        let t1_cur = get_coord(&self.zones[1].current[t1_idx]);
        let t2_orig = get_coord(&self.zones[1].original[t2_idx]);
        let t2_cur = get_coord(&self.zones[1].current[t2_idx]);

        let delta1 = t1_cur - t1_orig;
        let delta2 = t2_cur - t2_orig;

        // Walk from t1+1 to t2-1 (wrapping)
        if contour_len <= 1 {
            return;
        }

        let mut i = t1_idx;
        loop {
            // Advance to next point in contour (wrapping)
            i = if i == contour_end {
                contour_start
            } else {
                i + 1
            };

            if i == t2_idx {
                break;
            }

            if self.zones[1].flags[i].contains(touched_flag) {
                continue;
            }

            let orig = get_coord(&self.zones[1].original[i]);

            let new_coord = if t1_orig == t2_orig {
                // Both reference points are at the same position: shift
                let cur = get_coord(&self.zones[1].current[i]);
                cur + delta1
            } else {
                // Interpolate
                let lo_orig = t1_orig.min(t2_orig);
                let hi_orig = t1_orig.max(t2_orig);
                let lo_cur = if t1_orig < t2_orig { t1_cur } else { t2_cur };
                let hi_cur = if t1_orig < t2_orig { t2_cur } else { t1_cur };
                let lo_delta = if t1_orig < t2_orig { delta1 } else { delta2 };
                let hi_delta = if t1_orig < t2_orig { delta2 } else { delta1 };

                if orig <= lo_orig {
                    // Below lower bound: shift by lower delta
                    orig + lo_delta
                } else if orig >= hi_orig {
                    // Above upper bound: shift by upper delta
                    orig + hi_delta
                } else {
                    // Between: linear interpolation
                    let range = hi_orig - lo_orig;
                    let factor = orig - lo_orig;
                    lo_cur
                        + ((factor as i64 * (hi_cur - lo_cur) as i64
                            + (range as i64 / 2))
                            / range as i64) as i32
                }
            };

            if axis == 1 {
                self.zones[1].current[i].x = new_coord;
            } else {
                self.zones[1].current[i].y = new_coord;
            }
        }
    }

    // ── Coordinate / measurement ─────────────────────────────────────

    fn op_gc(&mut self, use_original: bool) -> Result<(), HintError> {
        let p = self.pop()? as u32;
        let zp2 = self.gs.zp2 as usize;
        let point = if use_original {
            self.get_original_point(zp2, p)?
        } else {
            self.get_point(zp2, p)?
        };
        let val = self.project(point);
        self.push(val)?;
        Ok(())
    }

    fn op_scfs(&mut self) -> Result<(), HintError> {
        let val = self.pop()?; // F26Dot6
        let p = self.pop()? as u32;
        let zp2 = self.gs.zp2 as usize;

        let point = self.get_point(zp2, p)?;
        let cur = self.project(point);
        self.move_point(zp2, p as usize, val - cur);
        Ok(())
    }

    fn op_md(&mut self, use_original: bool) -> Result<(), HintError> {
        let p2 = self.pop()? as u32;
        let p1 = self.pop()? as u32;

        let dist = if use_original {
            let p1_pt = self.get_original_point(self.gs.zp0 as usize, p1)?;
            let p2_pt = self.get_original_point(self.gs.zp1 as usize, p2)?;
            self.dual_project(Point {
                x: p2_pt.x - p1_pt.x,
                y: p2_pt.y - p1_pt.y,
            })
        } else {
            let p1_pt = self.get_point(self.gs.zp0 as usize, p1)?;
            let p2_pt = self.get_point(self.gs.zp1 as usize, p2)?;
            self.project(Point {
                x: p2_pt.x - p1_pt.x,
                y: p2_pt.y - p1_pt.y,
            })
        };

        self.push(dist)?;
        Ok(())
    }

    // ── Intersection ─────────────────────────────────────────────────

    fn op_isect(&mut self) -> Result<(), HintError> {
        let b1 = self.pop()? as u32;
        let b0 = self.pop()? as u32;
        let a1 = self.pop()? as u32;
        let a0 = self.pop()? as u32;
        let p = self.pop()? as u32;

        let pa0 = self.get_point(self.gs.zp1 as usize, a0)?;
        let pa1 = self.get_point(self.gs.zp1 as usize, a1)?;
        let pb0 = self.get_point(self.gs.zp0 as usize, b0)?;
        let pb1 = self.get_point(self.gs.zp0 as usize, b1)?;

        // Line A: pa0 to pa1, Line B: pb0 to pb1
        let dax = (pa1.x - pa0.x) as i64;
        let day = (pa1.y - pa0.y) as i64;
        let dbx = (pb1.x - pb0.x) as i64;
        let dby = (pb1.y - pb0.y) as i64;

        let denom = dax * dby - day * dbx;

        let zp2 = self.gs.zp2 as usize;
        let i = p as usize;
        if i >= self.zones[zp2].current.len() {
            return Err(HintError::InvalidPointIndex(p));
        }

        if denom.abs() < 1 {
            // Lines are parallel; use midpoint of endpoints
            self.zones[zp2].current[i].x = (pa0.x + pa1.x + pb0.x + pb1.x) / 4;
            self.zones[zp2].current[i].y = (pa0.y + pa1.y + pb0.y + pb1.y) / 4;
        } else {
            let dpx = (pb0.x - pa0.x) as i64;
            let dpy = (pb0.y - pa0.y) as i64;
            let t = (dpx * dby - dpy * dbx) * 64 / denom;

            self.zones[zp2].current[i].x = pa0.x + ((dax * t + 32) >> 6) as i32;
            self.zones[zp2].current[i].y = pa0.y + ((day * t + 32) >> 6) as i32;
        }

        self.zones[zp2].flags[i].insert(PointFlags::TOUCHED_X | PointFlags::TOUCHED_Y);
        Ok(())
    }

    // ── Flip ─────────────────────────────────────────────────────────

    fn op_flippt(&mut self) -> Result<(), HintError> {
        let loop_count = self.gs.loop_value;
        self.gs.loop_value = 1;

        for _ in 0..loop_count {
            let p = self.pop()? as usize;
            if let Some(flags) = self.zones[1].flags.get_mut(p) {
                flags.toggle(PointFlags::ON_CURVE);
            }
        }
        Ok(())
    }

    // ── Delta instructions ───────────────────────────────────────────

    fn op_deltap(
        &mut self,
        range: u8,
        _bytecode: &[u8],
        _ip: &mut usize,
    ) -> Result<(), HintError> {
        let n = self.pop()? as u32;
        let zp0 = self.gs.zp0 as usize;
        let delta_base = self.gs.delta_base as i32;
        let delta_shift = self.gs.delta_shift as i32;

        let range_offset = match range {
            1 => 0,
            2 => 16,
            3 => 32,
            _ => 0,
        };

        for _ in 0..n {
            let arg = self.pop()? as u32;
            let point_idx = self.pop()? as u32;

            let ppem_offset = ((arg >> 4) & 0x0F) as i32;
            let target_ppem = delta_base + range_offset + ppem_offset;

            if target_ppem == self.ppem as i32 {
                let magnitude = (arg & 0x0F) as i32;
                let delta = if magnitude < 8 {
                    -(magnitude + 1)
                } else {
                    magnitude - 7
                };
                // Scale by 1 / (1 << delta_shift)
                let scaled = if delta_shift > 0 {
                    delta * 64 / (1 << delta_shift)
                } else {
                    delta * 64
                };
                self.move_point(zp0, point_idx as usize, scaled);
            }
        }
        Ok(())
    }

    fn op_deltac(&mut self, range: u8) -> Result<(), HintError> {
        let n = self.pop()? as u32;
        let delta_base = self.gs.delta_base as i32;
        let delta_shift = self.gs.delta_shift as i32;

        let range_offset = match range {
            1 => 0,
            2 => 16,
            3 => 32,
            _ => 0,
        };

        for _ in 0..n {
            let arg = self.pop()? as u32;
            let cvt_idx = self.pop()? as u32;

            let ppem_offset = ((arg >> 4) & 0x0F) as i32;
            let target_ppem = delta_base + range_offset + ppem_offset;

            if target_ppem == self.ppem as i32 {
                let magnitude = (arg & 0x0F) as i32;
                let delta = if magnitude < 8 {
                    -(magnitude + 1)
                } else {
                    magnitude - 7
                };
                let scaled = if delta_shift > 0 {
                    delta * 64 / (1 << delta_shift)
                } else {
                    delta * 64
                };

                let i = cvt_idx as usize;
                if i < self.cvt.len() {
                    self.cvt[i] += scaled;
                }
            }
        }
        Ok(())
    }

    // ── GETINFO ──────────────────────────────────────────────────────

    fn op_getinfo(&mut self) -> Result<(), HintError> {
        let selector = self.pop()? as u32;
        let mut result = 0u32;

        // Bit 0: engine version
        if selector & 1 != 0 {
            // Return version 40 (Windows DirectWrite / modern rasterizer)
            result |= 40;
        }

        // Bit 1: glyph rotated (we don't rotate, so false)
        // Bit 2: glyph stretched (we don't stretch, so false)

        // Bit 3: font variations active
        // (not currently supported in our interpreter)

        // Bit 5: grayscale rendering
        if selector & (1 << 5) != 0 {
            result |= 1 << 12; // grayscale bit
        }

        // Bit 6: ClearType enabled
        if selector & (1 << 6) != 0 {
            result |= 1 << 13; // ClearType enabled
        }

        self.push(result as i32)?;
        Ok(())
    }
}
