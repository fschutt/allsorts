//! Parses glyph outlines, implementation adapted from
//! the owned-ttf-parser repository with authors permission
//!
//! ```
//!  Copyright 2020 Alex Butler
//!
//!  Licensed under the Apache License, Version 2.0 (the "License");
//!  you may not use this file except in compliance with the License.
//!  You may obtain a copy of the License at
//!
//!      http://www.apache.org/licenses/LICENSE-2.0
//!
//!  Unless required by applicable law or agreed to in writing, software
//!  distributed under the License is distributed on an "AS IS" BASIS,
//!  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//!  See the License for the specific language governing permissions and
//!  limitations under the License.
//! ```

/// A trait for glyph outline construction.
trait OutlineBuilder {
	/// Appends a MoveTo segment.
	///
	/// Start of a contour.
	fn move_to(&mut self, x: f32, y: f32);

	/// Appends a LineTo segment.
	fn line_to(&mut self, x: f32, y: f32);

	/// Appends a QuadTo segment.
	fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);

	/// Appends a CurveTo segment.
	fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);

	/// Appends a ClosePath segment.
	///
	/// End of a contour.
	fn close(&mut self);
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct Outline {
	pub operations: Vec<Operation>,
	pub bounding_rect: OutlineBoundingRect,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct OutlineBoundingRect {
	pub max_x: i16,
	pub max_y: i16,
	pub min_x: i16,
	pub min_y: i16,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum OutlineParseError {
	NoTablesFound,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum Operation {
	MoveTo { x: i16, y: i16 },
	LineTo { x: i16, y: i16 },
	QuadraticCurveTo { x: i16, y: i16 },
	CubicCurveTo { x: i16, y: i16 },
	ClosePath,
}

struct OutlineBuilder {
	outline: Outline,
}

/// Outlines a glyph and returns its tight bounding box.
///
/// Warning: since ttf-parser is a pull parser, OutlineBuilder will emit
/// segments even when outline is partially malformed. You must check
/// outline_glyph() result before using OutlineBuilder's output.
///
/// gvar, glyf, CFF and CFF2 tables are supported. And they will be
/// accesses in this specific order.
///
/// This method is affected by variation axes.
///
/// Note: does NOT support variable-weight fonts
fn parse_glyph_outline<'a, &T: OutlineBuilder>(data: &GlyphData<'a>, builder: &mut T)
-> Result<Vec<Operation>, OutlineParseError> {

	// if let Some(ref gvar_table) = self.gvar {
	//     return self::gvar::outline(data, self.loca?, self.glyf?, gvar_table, self.coords(), glyph_id, builder);
	// }

	if let Some(glyf_table) = self.glyf {
		match data {

		}

		// return self::glyf::outline(data, self.loca?, glyf_table, glyph_id, builder);
	}

	// if let Some(ref metadata) = self.cff1 {
	//     return self::cff1::outline(data, metadata, glyph_id, builder);
	// }

	// if let Some(ref metadata) = self.cff2 {
	//     return self::cff2::outline(data, metadata, self.coords(), glyph_id, builder);
	// }

	Err(OutlineParseError::NoTablesFound)
}

mod glyf {
	pub(in super) fn outline(
			loca_table: loca::Table,
			glyf_table: &[u8],
			glyph_id: GlyphId,
			builder: &mut dyn OutlineBuilder,
	) -> Option<Rect> {
			let mut b = Builder::new(Transform::default(), None, builder);
			let range = loca_table.glyph_range(glyph_id)?;
			let glyph_data = glyf_table.get(range)?;
			outline_impl(loca_table, glyf_table, glyph_data, 0, &mut b)
	}

	#[inline]
	fn outline_impl(
			loca_table: loca::Table,
			glyf_table: &[u8],
			data: &[u8],
			depth: u8,
			builder: &mut Builder,
	) -> Option<Rect> {
			if depth >= MAX_COMPONENTS { return None; }

			let mut s = Stream::new(data);
			let number_of_contours: i16 = s.read()?;
			// It's faster to parse the rect directly, instead of using `FromData`.
			let rect = Rect {
					x_min: s.read::<i16>()?,
					y_min: s.read::<i16>()?,
					x_max: s.read::<i16>()?,
					y_max: s.read::<i16>()?,
			};

			if number_of_contours > 0 {
				// Simple glyph.

				// u16 casting is safe, since we already checked that the value is positive.
				let number_of_contours = NonZeroU16::new(number_of_contours as u16)?;
				for point in parse_simple_outline(s.tail()?, number_of_contours)? {
					builder.push_point(f32::from(point.x), f32::from(point.y), point.on_curve_point, point.last_point);
				}
			} else if number_of_contours < 0 {
				// Composite glyph.
				for comp in CompositeGlyphIter::new(s.tail()?) {
					if let Some(range) = loca_table.glyph_range(comp.glyph_id) {
						if let Some(glyph_data) = glyf_table.get(range) {
							let transform = Transform::combine(builder.transform, comp.transform);
							let mut b = Builder::new(transform, None, builder.builder);
							outline_impl(loca_table, glyf_table, glyph_data, depth + 1, &mut b)?;
						}
					}
				}
			} else {
				// An empty glyph.
				return None;
			}

			Some(rect)
	}

	#[inline]
	pub fn parse_simple_outline(glyph_data: &[u8], number_of_contours: NonZeroU16) -> Option<GlyphPointsIter> {

		let mut s = Stream::new(glyph_data);
		let endpoints = s.read_array16::<u16>(number_of_contours.get())?;

		let points_total = endpoints.last()?.checked_add(1)?;

		// Contours with a single point should be ignored.
		// But this is not an error, so we should return an "empty" iterator.
		if points_total == 1 {
				return Some(GlyphPointsIter::default());
		}

		// Skip instructions byte code.
		let instructions_len: u16 = s.read()?;
		s.advance(usize::from(instructions_len));

		let flags_offset = s.offset();
		let (x_coords_len, y_coords_len) = resolve_coords_len(&mut s, points_total)?;
		let x_coords_offset = s.offset();
		let y_coords_offset = x_coords_offset + usize::num_from(x_coords_len);
		let y_coords_end = y_coords_offset + usize::num_from(y_coords_len);

		Some(GlyphPointsIter {
				endpoints: EndpointsIter::new(endpoints)?,
				flags: FlagsIter::new(glyph_data.get(flags_offset..x_coords_offset)?),
				x_coords: CoordsIter::new(glyph_data.get(x_coords_offset..y_coords_offset)?),
				y_coords: CoordsIter::new(glyph_data.get(y_coords_offset..y_coords_end)?),
				points_left: points_total,
		})
	}
}
