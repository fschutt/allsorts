#![deny(missing_docs)]

//! Common tables pertaining to variable fonts.

use std::borrow::Cow;
use std::convert::{TryFrom, TryInto};
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::{fmt, iter};
use tinyvec::TinyVec;

use crate::binary::read::{
    ReadArray, ReadBinary, ReadBinaryDep, ReadCtxt, ReadFixedSizeDep, ReadFrom, ReadScope,
    ReadUnchecked,
};
use crate::binary::{I16Be, I32Be, U16Be, U32Be, I8, U8};
use crate::error::ParseError;
use crate::tables::variable_fonts::gvar::{GvarTable, NumPoints};
use crate::tables::{F2Dot14, Fixed};
use crate::SafeFrom;

pub mod avar;
pub mod cvar;
pub mod fvar;
pub mod gvar;
pub mod mvar;
pub mod stat;

// TODO: Move constants into structs they relate to

/// Flag indicating that some or all tuple variation tables reference a shared set of “point”
/// numbers.
///
/// These shared numbers are represented as packed point number data at the start of the serialized
/// data.
const SHARED_POINT_NUMBERS: u16 = 0x8000;
/// Mask for the low bits to give the number of tuple variation tables.
const COUNT_MASK: u16 = 0x0FFF;
/// Flag indicating the data type used for point numbers in this run.
///
/// If set, the point numbers are stored as unsigned 16-bit values (uint16); if clear, the point
/// numbers are stored as unsigned bytes (uint8).
const POINTS_ARE_WORDS: u8 = 0x80;
/// Mask for the low 7 bits of the control byte to give the number of point number elements, minus
/// 1.
const POINT_RUN_COUNT_MASK: u8 = 0x7F;

/// Flag indicating that this tuple variation header includes an embedded peak tuple record,
/// immediately after the tupleIndex field.
///
/// If set, the low 12 bits of the tupleIndex value are ignored.
///
/// Note that this must always be set within the `cvar` table.
const EMBEDDED_PEAK_TUPLE: u16 = 0x8000;
/// Flag indicating that this tuple variation table applies to an intermediate region within the
/// variation space.
///
/// If set, the header includes the two intermediate-region, start and end tuple records,
/// immediately after the peak tuple record (if present).
const INTERMEDIATE_REGION: u16 = 0x4000;
/// Flag indicating that the serialized data for this tuple variation table includes packed “point”
/// number data.
///
/// If set, this tuple variation table uses that number data; if clear, this tuple variation table
/// uses shared number data found at the start of the serialized data for this glyph variation data
/// or 'cvar' table.
const PRIVATE_POINT_NUMBERS: u16 = 0x2000;
/// Mask for the low 12 bits to give the shared tuple records index.
const TUPLE_INDEX_MASK: u16 = 0x0FFF;

/// Flag indicating that this run contains no data (no explicit delta values are stored), and that
/// the deltas for this run are all zero.
const DELTAS_ARE_ZERO: u8 = 0x80;
/// Flag indicating the data type for delta values in the run.
///
/// If set, the run contains 16-bit signed deltas (int16); if clear, the run contains 8-bit signed
/// deltas (int8).
const DELTAS_ARE_WORDS: u8 = 0x40;
/// Mask for the low 6 bits to provide the number of delta values in the run, minus one.
const DELTA_RUN_COUNT_MASK: u8 = 0x3F;

/// Coordinate array specifying a position within the font’s variation space.
///
/// The number of elements must match the axisCount specified in the `fvar` table.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#tuple-records>
// pub type Tuple<'a> = ReadArray<'a, F2Dot14>;
#[derive(Debug, Clone)]
pub struct Tuple<'a>(pub(crate) ReadArray<'a, F2Dot14>);

/// Tuple in user coordinates
///
/// **Note:** The UserTuple record and Tuple record both describe a position in the variation space
/// but are distinct: UserTuple uses Fixed values to represent user scale coordinates, while Tuple
/// record uses F2DOT14 values to reporesent normalized coordinates.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/fvar#instancerecord>
#[derive(Debug)]
pub struct UserTuple<'a>(pub(crate) ReadArray<'a, Fixed>);

// TODO: Make this a new-type like the others
pub(crate) type OwnedTuple = TinyVec<[F2Dot14; 4]>;

/// Phantom type for TupleVariationStore from a `gvar` table.
pub enum Gvar {}
/// Phantom type for TupleVariationStore from a `CVT` table.
pub enum Cvar {}

/// Tuple Variation Store Header.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#tuple-variation-store-header>
pub struct TupleVariationStore<'a, T> {
    /// The number of points in the glyph this store is for
    num_points: u32,
    /// A packed field. The high 4 bits are flags, and the low 12 bits are the number
    /// of tuple variation tables. The count can be any number between 1 and 4095.
    tuple_variation_flags_and_count: u16,
    /// Offset from the start of the table containing the tuple store to the serialized data.
    data_offset: u16,
    /// The serialized data block begins with shared “point” number data, followed by the variation
    /// data for the tuple variation tables.
    ///
    /// The shared point number data is optional: it is present if the corresponding flag is set in
    /// the `tuple_variation_flags_and_count` field of the header.
    shared_point_numbers: Option<PointNumbers>,
    /// Array of tuple variation headers.
    tuple_variation_headers: Vec<TupleVariationHeader<'a, T>>,
}

/// Tuple variation header.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#tuplevariationheader>
pub struct TupleVariationHeader<'a, T> {
    /// The size in bytes of the serialized data for this tuple variation table.
    variation_data_size: u16,
    /// A packed field. The high 4 bits are flags. The low 12 bits are an index into a
    /// shared tuple records array.
    tuple_flags_and_index: u16,
    /// Peak tuple record for this tuple variation table — optional, determined by flags in the
    /// tupleIndex value.
    ///
    /// Note that this must always be included in the `cvar` table.
    peak_tuple: Option<Tuple<'a>>,
    /// The start and end tuples for the intermediate region.
    ///
    /// Presence determined by flags in the `tuple_flags_and_index` value.
    intermediate_region: Option<(Tuple<'a>, Tuple<'a>)>,
    /// The serialized data for this Tuple Variation
    data: &'a [u8],
    variant: PhantomData<T>,
}

/// Glyph variation data.
///
/// (x, y) deltas for numbered points.
pub struct GvarVariationData<'a> {
    point_numbers: Cow<'a, PointNumbers>,
    x_coord_deltas: Vec<i16>,
    y_coord_deltas: Vec<i16>,
}

/// CVT variation data.
///
/// deltas for numbered CVTs.
pub struct CvarVariationData<'a> {
    point_numbers: Cow<'a, PointNumbers>,
    deltas: Vec<i16>,
}

#[derive(Clone)]
enum PointNumbers {
    All(u32),
    Specific(Vec<u16>),
}

/// A collection of point numbers that are shared between variations.
pub struct SharedPointNumbers<'a>(&'a PointNumbers);

/// Item variation store.
///
/// > Includes a variation region list, which defines the different regions of the font’s variation
/// > space for which variation data is defined. It also includes a set of itemVariationData
/// > sub-tables, each of which provides a portion of the total variation data. Each sub-table is
/// > associated with some subset of the defined regions, and will include deltas used for one or
/// > more target items.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#variation-data>
pub struct ItemVariationStore<'a> {
    /// The variation region list.
    variation_region_list: VariationRegionList<'a>,
    /// The item variation data
    item_variation_data: Vec<ItemVariationData<'a>>,
}

struct VariationRegionList<'a> {
    /// The number of variation axes for this font. This must be the same number as axisCount in
    /// the `fvar` table.
    axis_count: u16,
    /// Array of variation regions.
    variation_regions: ReadArray<'a, VariationRegion<'a>>,
}

struct ItemVariationData<'a> {
    /// The number of delta sets for distinct items.
    item_count: u16,
    /// A packed field: the high bit is a flag.
    word_delta_count: u16,
    /// The number of variation regions referenced.
    region_index_count: u16,
    /// Array of indices into the variation region list for the regions referenced by this item
    /// variation data table.
    region_indexes: ReadArray<'a, U16Be>,
    /// Delta-set rows.
    delta_sets: &'a [u8],
}

pub(crate) struct VariationRegion<'a> {
    /// Array of region axis coordinates records, in the order of axes given in the `fvar` table.
    region_axes: ReadArray<'a, RegionAxisCoordinates>,
}

struct RegionAxisCoordinates {
    /// The region start coordinate value for the current axis.
    start_coord: F2Dot14,
    /// The region peak coordinate value for the current axis.
    peak_coord: F2Dot14,
    /// The region end coordinate value for the current axis.
    end_coord: F2Dot14,
}

struct DeltaSetIndexMap<'a> {
    /// A packed field that describes the compressed representation of delta-set indices.
    entry_format: u8,
    /// The number of mapping entries.
    map_count: u32,
    /// The delta-set index mapping data.
    map_data: &'a [u8],
}

struct DeltaSetIndexMapEntry {
    /// Index into the outer table (row)
    outer_index: u16,
    /// Index into the inner table (column)
    inner_index: u16,
}

impl<'a> UserTuple<'a> {
    /// Iterate over the axis values in this user tuple.
    pub fn iter<'b: 'a>(&'b self) -> impl ExactSizeIterator<Item = Fixed> + 'a {
        self.0.iter()
    }

    /// Returns the number of values in this user tuple.
    ///
    /// Should be the same as the number of axes in the `fvar` table.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<'data, T> TupleVariationStore<'data, T> {
    /// Iterate over the tuple variation headers.
    pub fn headers(&self) -> impl Iterator<Item = &TupleVariationHeader<'data, T>> {
        self.tuple_variation_headers.iter()
    }

    /// Get the shared point numbers for this variation store if present.
    pub fn shared_point_numbers(&self) -> Option<SharedPointNumbers<'_>> {
        self.shared_point_numbers.as_ref().map(SharedPointNumbers)
    }
}

impl TupleVariationStore<'_, Gvar> {
    /// Retrieve the variation data for the variation tuple at the given index.
    pub fn variation_data(&self, index: u16) -> Result<GvarVariationData<'_>, ParseError> {
        let header = self
            .tuple_variation_headers
            .get(usize::from(index))
            .ok_or(ParseError::BadIndex)?;
        header.variation_data(
            NumPoints::from_raw(self.num_points),
            self.shared_point_numbers(),
        )
    }
}

impl<T> ReadBinaryDep for TupleVariationStore<'_, T> {
    type Args<'a> = (u16, u32);
    type HostType<'a> = TupleVariationStore<'a, T>;

    fn read_dep<'a>(
        ctxt: &mut ReadCtxt<'a>,
        (axis_count, num_points): (u16, u32),
    ) -> Result<Self::HostType<'a>, ParseError> {
        let axis_count = usize::from(axis_count);

        let scope = ctxt.scope();
        let tuple_variation_flags_and_count = ctxt.read_u16be()?;
        let tuple_variation_count = usize::from(tuple_variation_flags_and_count & COUNT_MASK);
        let data_offset = ctxt.read_u16be()?;

        // Now read the TupleVariationHeaders
        let mut tuple_variation_headers = (0..tuple_variation_count)
            .map(|_| ctxt.read_dep::<TupleVariationHeader<'_, T>>(axis_count))
            .collect::<Result<Vec<_>, _>>()?;

        // Read the serialized data for each tuple variation header
        let mut data_ctxt = scope.offset(data_offset.into()).ctxt(); // FIXME: into

        // Read shared point numbers if the flag indicates they are present
        let shared_point_numbers = ((tuple_variation_flags_and_count & SHARED_POINT_NUMBERS)
            == SHARED_POINT_NUMBERS)
            .then(|| read_packed_point_numbers(&mut data_ctxt, num_points))
            .transpose()?;

        // Populate the data slices on the headers
        for header in tuple_variation_headers.iter_mut() {
            header.data = data_ctxt.read_slice(header.variation_data_size.into())?;
        }

        Ok(TupleVariationStore {
            num_points,
            tuple_variation_flags_and_count,
            data_offset,
            shared_point_numbers,
            tuple_variation_headers,
        })
    }
}

impl PointNumbers {
    /// Returns the number of point numbers contained by this value
    pub fn len(&self) -> usize {
        match self {
            PointNumbers::All(n) => usize::safe_from(*n),
            PointNumbers::Specific(vec) => vec.len(),
        }
    }

    /// Iterate over the point numbers contained by this value.
    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.len()).map(move |index| {
            match self {
                // NOTE(cast): Safe as len is from `n`, which is a u32
                PointNumbers::All(_n) => index as u32,
                // NOTE(unwrap): Safe as index is bounded by `len`
                PointNumbers::Specific(numbers) => {
                    numbers.get(index).copied().map(u32::from).unwrap()
                }
            }
        })
    }
}

/// Read packed point numbers for a glyph with `num_points` points.
///
/// `num_points` is expected to already have the four "phantom points" added to it.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#packed-point-numbers>
fn read_packed_point_numbers(
    ctxt: &mut ReadCtxt<'_>,
    num_points: u32,
) -> Result<PointNumbers, ParseError> {
    let count = read_count(ctxt)?;
    // If the first byte is 0, then a second count byte is not used. This value has a special
    // meaning: the tuple variation data provides deltas for all glyph points (including the
    // “phantom” points), or for all CVTs.
    if count == 0 {
        return Ok(PointNumbers::All(num_points));
    }

    let mut num_read = 0;
    let mut point_numbers = Vec::with_capacity(usize::from(count));
    while num_read < count {
        let control_byte = ctxt.read_u8()?;
        let point_run_count = u16::from(control_byte & POINT_RUN_COUNT_MASK) + 1;
        if (control_byte & POINTS_ARE_WORDS) == POINTS_ARE_WORDS {
            // Points are words (2 bytes)
            let array = ctxt.read_array::<U16Be>(point_run_count.into())?;
            point_numbers.extend(array.iter().scan(0u16, |prev, diff| {
                let number = *prev + diff;
                *prev = number;
                Some(number)
            }));
        } else {
            // Points are single bytes
            let array = ctxt.read_array::<U8>(point_run_count.into())?;
            point_numbers.extend(array.iter().scan(0u16, |prev, diff| {
                let number = *prev + u16::from(diff);
                *prev = number;
                Some(number)
            }));
        }
        num_read += point_run_count;
    }
    Ok(PointNumbers::Specific(point_numbers))
}

// The count may be stored in one or two bytes:
//
// * If the first byte is 0, then a second count byte is not used. This value has a special
//   meaning: the tuple variation data provides deltas for all glyph points (including the “phantom”
//   points), or for all CVTs.
// * If the first byte is non-zero and the high bit is clear (value is 1 to 127), then a second
//   count byte is not used. The point count is equal to the value of the first byte.
// * If the high bit of the first byte is set, then a second byte is used. The count is read from
//   interpreting the two bytes as a big-endian uint16 value with the high-order bit masked out.
fn read_count(ctxt: &mut ReadCtxt<'_>) -> Result<u16, ParseError> {
    let count1 = u16::from(ctxt.read_u8()?);
    let count = match count1 {
        0 => 0,
        1..=127 => count1,
        128.. => {
            let count2 = ctxt.read_u8()?;
            ((count1 & 0x7F) << 8) | u16::from(count2)
        }
    };
    Ok(count)
}

/// Read `num_deltas` packed deltas.
///
/// <https://learn.microsoft.com/en-us/typography/opentype/spec/otvarcommonformats#packed-deltas>
fn read_packed_deltas(ctxt: &mut ReadCtxt<'_>, num_deltas: u32) -> Result<Vec<i16>, ParseError> {
    let mut deltas_read = 0;
    let mut deltas = Vec::with_capacity(usize::safe_from(num_deltas));

    while deltas_read < num_deltas {
        let control_byte = ctxt.read_u8()?;
        // FIXME: Handling of count (u16, u32, usize)
        let count = u16::from(control_byte & DELTA_RUN_COUNT_MASK) + 1; // value is stored - 1
        let ucount = usize::from(count);

        deltas.reserve(ucount);
        if (control_byte & DELTAS_ARE_ZERO) == DELTAS_ARE_ZERO {
            deltas.extend(iter::repeat(0).take(ucount));
        } else if (control_byte & DELTAS_ARE_WORDS) == DELTAS_ARE_WORDS {
            // Points are words (2 bytes)
            let array = ctxt.read_array::<I16Be>(ucount)?;
            deltas.extend(array.iter())
        } else {
            // Points are single bytes
            let array = ctxt.read_array::<I8>(ucount)?;
            deltas.extend(array.iter().map(i16::from));
        };
        deltas_read += u32::from(count);
    }

    Ok(deltas)
}

impl GvarVariationData<'_> {
    /// Iterates over the point numbers and (x, y) deltas.
    pub fn iter(&self) -> impl Iterator<Item = (u32, (i16, i16))> + '_ {
        let deltas = self
            .x_coord_deltas
            .iter()
            .copied()
            .zip(self.y_coord_deltas.iter().copied());
        self.point_numbers.iter().zip(deltas)
    }

    /// Returns the number of point numbers.
    pub fn len(&self) -> usize {
        self.point_numbers.len()
    }
}

impl<'data> TupleVariationHeader<'data, Gvar> {
    /// Read the variation data for `gvar`.
    ///
    /// `num_points` is the number of points in the glyph this variation relates to.
    pub fn variation_data<'a>(
        &'a self,
        num_points: NumPoints,
        shared_point_numbers: Option<SharedPointNumbers<'a>>,
    ) -> Result<GvarVariationData<'a>, ParseError> {
        let mut ctxt = ReadScope::new(self.data).ctxt();

        let point_numbers =
            self.read_point_numbers(&mut ctxt, num_points.get(), shared_point_numbers)?;
        let num_deltas = u32::try_from(point_numbers.len()).map_err(ParseError::from)?;

        // The deltas are stored X, followed by Y but the delta runs can span the boundary of the
        // two so they need to be read as a single span of packed deltas and then split.
        let mut x_coord_deltas = read_packed_deltas(&mut ctxt, 2 * num_deltas)?;
        let y_coord_deltas = x_coord_deltas.split_off(usize::safe_from(num_deltas));

        Ok(GvarVariationData {
            point_numbers,
            x_coord_deltas,
            y_coord_deltas,
        })
    }

    /// Returns the index of the shared tuple that this header relates to.
    ///
    /// The tuple index is an index into the shared tuples of the `Gvar` table. Pass this value
    /// to the [shared_tuple](gvar::GvarTable::shared_tuple) method to retrieve the tuple.
    ///
    /// The value returned from this method will be `None` if the header has an embedded
    /// peak tuple.
    pub fn tuple_index(&self) -> Option<u16> {
        self.peak_tuple
            .is_none()
            .then(|| self.tuple_flags_and_index & TUPLE_INDEX_MASK)
    }

    /// Returns the peak tuple for this tuple variation record.
    ///
    /// If the record contains an embedded peak tuple then that is returned, otherwise the
    /// referenced shared peak tuple is returned.
    pub fn peak_tuple<'a>(
        &'a self,
        gvar: &'a GvarTable<'data>,
    ) -> Result<Tuple<'data>, ParseError> {
        match self.peak_tuple.as_ref() {
            // NOTE(clone): cheap as Tuple is just a wrapper around ReadArray
            Some(tuple) => Ok(tuple.clone()),
            None => {
                let shared_index = self.tuple_flags_and_index & TUPLE_INDEX_MASK;
                gvar.shared_tuple(shared_index)
            }
        }
    }

    /// Returns the intermediate region of the tuple variation space that this variation applies to.
    ///
    /// If an intermediate region is not specified (the region is implied by the peak tuple) then
    /// this will be `None`.
    pub fn intermediate_region(&self) -> Option<(Tuple<'data>, Tuple<'data>)> {
        // NOTE(clone): Cheap as Tuple just contains ReadArray
        self.intermediate_region.clone()
    }
}

impl<'data> TupleVariationHeader<'data, Cvar> {
    /// Read the variation data for `cvar`.
    ///
    /// `num_cvts` is the number of CVTs in the CVT table.
    fn variation_data<'a>(
        &'a self,
        num_cvts: u32,
        shared_point_numbers: Option<SharedPointNumbers<'a>>,
    ) -> Result<CvarVariationData<'_>, ParseError> {
        let mut ctxt = ReadScope::new(self.data).ctxt();

        let point_numbers = self.read_point_numbers(&mut ctxt, num_cvts, shared_point_numbers)?;
        let num_deltas = u32::try_from(point_numbers.len()).map_err(ParseError::from)?;
        let deltas = read_packed_deltas(&mut ctxt, num_deltas)?;

        Ok(CvarVariationData {
            point_numbers,
            deltas,
        })
    }

    /// Returns the index of the shared tuple that this header relates to.
    ///
    /// The tuple index is an index into the shared tuples of the `Gvar` table. Pass this value
    /// to the [shared_tuple](gvar::GvarTable::shared_tuple) method to retrieve the tuple.
    ///
    /// The value returned from this method will be `None` if the header has an embedded
    /// peak tuple.
    pub fn tuple_index(&self) -> Option<u16> {
        self.peak_tuple
            .is_none()
            .then(|| self.tuple_flags_and_index & TUPLE_INDEX_MASK)
    }

    // FIXME: This is mandatory for Cvar
    /// Returns the embedded peak tuple if present.
    pub fn peak_tuple(&self) -> Option<&Tuple<'data>> {
        self.peak_tuple.as_ref()
    }
}

impl<'data, T> TupleVariationHeader<'data, T> {
    /// Read the point numbers for this tuple.
    ///
    /// This method will return either the embedded private point numbers or the shared numbers
    /// if private points are not present.
    fn read_point_numbers<'a>(
        &'a self,
        ctxt: &mut ReadCtxt<'data>,
        num_points: u32,
        shared_point_numbers: Option<SharedPointNumbers<'a>>,
    ) -> Result<Cow<'_, PointNumbers>, ParseError> {
        // Read private point numbers if the flag indicates they are present
        let private_point_numbers =
            if (self.tuple_flags_and_index & PRIVATE_POINT_NUMBERS) == PRIVATE_POINT_NUMBERS {
                read_packed_point_numbers(ctxt, num_points).map(Some)?
            } else {
                None
            };

        // If there are private point numbers then we need to read that many points
        // otherwise we need to read as many points are specified by the shared points.
        //
        // Either private or shared point numbers should be present. If both are missing that's
        // invalid.
        private_point_numbers
            .map(Cow::Owned)
            .or_else(|| shared_point_numbers.map(|shared| Cow::Borrowed(shared.0)))
            .ok_or(ParseError::MissingValue)
    }
}

impl<T> ReadBinaryDep for TupleVariationHeader<'_, T> {
    type Args<'a> = usize;
    type HostType<'a> = TupleVariationHeader<'a, T>;

    fn read_dep<'a>(
        ctxt: &mut ReadCtxt<'a>,
        axis_count: usize,
    ) -> Result<Self::HostType<'a>, ParseError> {
        // The size in bytes of the serialized data for this tuple variation table.
        let variation_data_size = ctxt.read_u16be()?;
        // A packed field. The high 4 bits are flags. The low 12 bits are an index into a
        // shared tuple records array.
        let tuple_flags_and_index = ctxt.read_u16be()?;
        // If this is absent then `tuple_flags_and_index` contains the index to one of the shared
        // tuple records to use instead:
        //
        // > Every tuple variation table has a peak n-tuple indicated either by an embedded tuple
        // > record (always true in the 'cvar' table) or by an index into a shared tuple records
        // > array (only in the 'gvar' table).
        // FIXME: This is not optional for Cvar
        let peak_tuple = ((tuple_flags_and_index & EMBEDDED_PEAK_TUPLE) == EMBEDDED_PEAK_TUPLE)
            .then(|| ctxt.read_array(axis_count).map(Tuple))
            .transpose()?;
        let intermediate_region =
            if (tuple_flags_and_index & INTERMEDIATE_REGION) == INTERMEDIATE_REGION {
                let start = ctxt.read_array(axis_count).map(Tuple)?;
                let end = ctxt.read_array(axis_count).map(Tuple)?;
                Some((start, end))
            } else {
                None
            };
        Ok(TupleVariationHeader {
            variation_data_size,
            tuple_flags_and_index,
            peak_tuple,
            intermediate_region,
            data: &[], // filled in later
            variant: PhantomData,
        })
    }
}

impl fmt::Debug for TupleVariationHeader<'_, Gvar> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("TupleVariationHeader");
        match &self.peak_tuple {
            Some(peak) => debug_struct.field("peak_tuple", peak),
            None => debug_struct.field("shared_tuple_index", &self.tuple_index()),
        };
        debug_struct
            .field("intermediate_region", &self.intermediate_region)
            .finish()
    }
}

impl<'a> ItemVariationStore<'a> {
    pub(crate) fn variation_region(&self, region_index: u16) -> Option<VariationRegion<'a>> {
        let region_index = usize::from(region_index);
        if region_index >= self.variation_region_list.variation_regions.len() {
            return None;
        }
        self.variation_region_list
            .variation_regions
            .read_item(region_index)
            .ok()
    }
}

impl ReadBinary for ItemVariationStore<'_> {
    type HostType<'a> = ItemVariationStore<'a>;

    fn read<'a>(ctxt: &mut ReadCtxt<'a>) -> Result<Self::HostType<'a>, ParseError> {
        let scope = ctxt.scope();
        let format = ctxt.read_u16be()?;
        ctxt.check(format == 1)?;
        let variation_region_list_offset = ctxt.read_u32be()?;
        let item_variation_data_count = ctxt.read_u16be()?;
        let item_variation_data_offsets =
            ctxt.read_array::<U32Be>(usize::from(item_variation_data_count))?;
        let variation_region_list = scope
            .offset(usize::safe_from(variation_region_list_offset))
            .read::<VariationRegionList<'_>>()?;
        let item_variation_data = item_variation_data_offsets
            .iter()
            .map(|offset| {
                scope
                    .offset(usize::safe_from(offset))
                    .read::<ItemVariationData<'_>>()
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ItemVariationStore {
            variation_region_list,
            item_variation_data,
        })
    }
}

impl ReadBinary for VariationRegionList<'_> {
    type HostType<'a> = VariationRegionList<'a>;

    fn read<'a>(ctxt: &mut ReadCtxt<'a>) -> Result<Self::HostType<'a>, ParseError> {
        let axis_count = ctxt.read_u16be()?;
        let region_count = ctxt.read_u16be()?;
        // The high-order bit of the region_count field is reserved for future use,
        // and must be cleared.
        ctxt.check(region_count < 32768)?;
        let variation_regions = ctxt.read_array_dep(usize::from(region_count), axis_count)?;
        Ok(VariationRegionList {
            axis_count,
            variation_regions,
        })
    }
}

// In general, variation deltas are, logically, signed 16-bit integers, and in most cases, they are applied to signed 16-bit values
// The LONG_WORDS flag should only be used in top-level tables that include 32-bit values that can be variable — currently, only the COLR table.
struct DeltaSet<'a> {
    long_deltas: bool,
    word_data: &'a [u8],
    short_data: &'a [u8],
}

impl<'a> DeltaSet<'a> {
    fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        // NOTE(unwrap): Safe as `mid` is multiple of U32Be::SIZE
        let (short_size, long_size) = if self.long_deltas {
            (I16Be::SIZE, I32Be::SIZE)
        } else {
            (I8::SIZE, I16Be::SIZE)
        };
        let words = self.word_data.chunks(long_size).map(move |chunk| {
            if self.long_deltas {
                i32::from_be_bytes(chunk.try_into().unwrap())
            } else {
                i32::from(i16::from_be_bytes(chunk.try_into().unwrap()))
            }
        });
        let shorts = self.short_data.chunks(short_size).map(move |chunk| {
            if self.long_deltas {
                i32::from(i16::from_be_bytes(chunk.try_into().unwrap()))
            } else {
                i32::from(chunk[0] as i8)
            }
        });

        words.chain(shorts)
    }
}

struct LongDeltaSet<'a> {
    word_data: &'a [u8],
    short_data: &'a [u8],
}

struct RegularDeltaSet<'a> {
    word_data: &'a [u8],
    short_data: &'a [u8],
}

impl<'a> RegularDeltaSet<'a> {
    pub fn iter(&self) -> impl Iterator<Item = i16> + '_ {
        // NOTE(unwrap): Safe as `mid` is multiple of U16Be::SIZE
        let words = self
            .word_data
            .chunks(I16Be::SIZE)
            .map(|chunk| i16::from_be_bytes(chunk.try_into().unwrap()));
        let shorts = self.short_data.iter().copied().map(i16::from);

        words.chain(shorts)
    }
}

impl<'a> LongDeltaSet<'a> {
    fn iter(&self) -> impl Iterator<Item = i32> + '_ {
        // NOTE(unwrap): Safe as `mid` is multiple of U32Be::SIZE
        let words = self
            .word_data
            .chunks(I32Be::SIZE)
            .map(|chunk| i32::from_be_bytes(chunk.try_into().unwrap()));
        let shorts = self
            .short_data
            .chunks(I16Be::SIZE)
            .map(|chunk| i32::from(i16::from_be_bytes(chunk.try_into().unwrap())));

        words.chain(shorts)
    }
}

impl ItemVariationData<'_> {
    /// Flag indicating that “word” deltas are long (int32)
    const LONG_WORDS: u16 = 0x8000;
    /// Count of “word” deltas
    const WORD_DELTA_COUNT_MASK: u16 = 0x7FFF;

    /// Retrieve a delta-set row within this item variation data sub-table.
    pub fn delta_set(&self, index: u16) -> Option<DeltaSet<'_>> {
        let row_length = self.row_length();
        let row_data = self
            .delta_sets
            .get(usize::from(index) * row_length..)
            .and_then(|offset| offset.get(..row_length))?;
        let mid = self.word_delta_count() * self.word_delta_size();
        if mid > row_data.len() {
            return None;
        }
        let (word_data, short_data) = row_data.split_at(mid);

        // Check that short data is a multiple of the short size
        if short_data.len() % self.short_delta_size() != 0 {
            return None;
        }

        Some(DeltaSet {
            long_deltas: self.long_deltas(),
            word_data,
            short_data,
        })
    }

    fn word_delta_size(&self) -> usize {
        if self.long_deltas() {
            I32Be::SIZE
        } else {
            I16Be::SIZE
        }
    }

    fn short_delta_size(&self) -> usize {
        if self.long_deltas() {
            I16Be::SIZE
        } else {
            U8::SIZE
        }
    }

    fn row_length(&self) -> usize {
        Self::row_length_impl(self.region_index_count, self.word_delta_count)
    }

    fn row_length_impl(region_index_count: u16, word_delta_count: u16) -> usize {
        let row_length = usize::from(region_index_count)
            + usize::from(word_delta_count & Self::WORD_DELTA_COUNT_MASK);
        if word_delta_count & Self::LONG_WORDS == 0 {
            row_length
        } else {
            row_length * 2
        }
    }

    fn word_delta_count(&self) -> usize {
        usize::from(self.word_delta_count & Self::WORD_DELTA_COUNT_MASK)
    }

    fn long_deltas(&self) -> bool {
        self.word_delta_count & Self::LONG_WORDS != 0
    }
}

impl ReadBinary for ItemVariationData<'_> {
    type HostType<'a> = ItemVariationData<'a>;

    fn read<'a>(ctxt: &mut ReadCtxt<'a>) -> Result<Self::HostType<'a>, ParseError> {
        let item_count = ctxt.read_u16be()?;
        let word_delta_count = ctxt.read_u16be()?;
        let region_index_count = ctxt.read_u16be()?;
        let region_indexes = ctxt.read_array::<U16Be>(usize::from(region_index_count))?;
        let row_length = Self::row_length_impl(region_index_count, word_delta_count);
        let delta_sets = ctxt.read_slice(usize::from(item_count) * row_length)?;

        Ok(ItemVariationData {
            item_count,
            word_delta_count,
            region_index_count,
            region_indexes,
            delta_sets,
        })
    }
}

impl<'a> VariationRegion<'a> {
    pub(crate) fn scalar(&self, tuple: impl Iterator<Item = F2Dot14>) -> Option<f32> {
        let scalar = self
            .region_axes
            .iter()
            .zip(tuple)
            .map(|(region, instance)| {
                // FIXME extract this body to a function that can be used by gvar too

                let RegionAxisCoordinates {
                    start_coord: start,
                    peak_coord: peak,
                    end_coord: end,
                } = region;
                // If peak is zero or not contained by the region of applicability then it does not
                if peak == F2Dot14::from(0) {
                    // If the peak is zero for some axis, then ignore the axis.
                    1.
                } else if (start..=end).contains(&instance) {
                    // The region is applicable: calculate a per-axis scalar as a proportion
                    // of the proximity of the instance to the peak within the region.
                    if instance == peak {
                        1.
                    } else if instance < peak {
                        (f32::from(instance) - f32::from(start))
                            / (f32::from(peak) - f32::from(start))
                    } else {
                        // instance > peak
                        (f32::from(end) - f32::from(instance)) / (f32::from(end) - f32::from(peak))
                    }
                } else {
                    // If the instance coordinate is out of range for some axis, then the region and its
                    // associated deltas are not applicable.
                    0.
                }
            })
            .fold(1., |scalar, axis_scalar| scalar * axis_scalar);

        // FIXME: This comparison is dubious; make better
        (scalar != 0.).then(|| scalar)
    }
}

impl ReadBinaryDep for VariationRegion<'_> {
    type Args<'a> = u16;
    type HostType<'a> = VariationRegion<'a>;

    fn read_dep<'a>(
        ctxt: &mut ReadCtxt<'a>,
        axis_count: u16,
    ) -> Result<Self::HostType<'a>, ParseError> {
        let region_axes = ctxt.read_array(usize::from(axis_count))?;
        Ok(VariationRegion { region_axes })
    }
}

impl ReadFixedSizeDep for VariationRegion<'_> {
    fn size(axis_count: u16) -> usize {
        usize::from(axis_count) * RegionAxisCoordinates::SIZE
    }
}

impl ReadFrom for RegionAxisCoordinates {
    type ReadType = (F2Dot14, F2Dot14, F2Dot14);

    fn read_from((start_coord, peak_coord, end_coord): (F2Dot14, F2Dot14, F2Dot14)) -> Self {
        RegionAxisCoordinates {
            start_coord,
            peak_coord,
            end_coord,
        }
    }
}

impl DeltaSetIndexMap<'_> {
    /// Mask for the low 4 bits of the DeltaSetIndexMap entry format.
    ///
    /// Gives the count of bits minus one that are used in each entry for the inner-level index.
    const INNER_INDEX_BIT_COUNT_MASK: u8 = 0x0F;
    /// Mask for bits of the DeltaSetIndexMap entry format that indicate the size in bytes minus
    /// one of each entry.
    const MAP_ENTRY_SIZE_MASK: u8 = 0x30;

    /// Returns delta-set outer-level index and inner-level index combination.
    pub fn entry(&self, i: u32) -> Result<DeltaSetIndexMapEntry, ParseError> {
        let entry_size = usize::from(self.entry_size());
        let offset = usize::safe_from(i) * entry_size;
        let entry_bytes = self
            .map_data
            .get(offset..(offset + entry_size))
            .ok_or_else(|| ParseError::BadIndex)?;

        // entry can be 1, 2, 3, or 4 bytes
        let entry = entry_bytes
            .iter()
            .copied()
            .fold(0u32, |entry, byte| (entry << 8) | u32::from(byte));
        let outer_index =
            (entry >> (u32::from(self.entry_format & Self::INNER_INDEX_BIT_COUNT_MASK) + 1)) as u16;
        let inner_index = (entry
            & ((1 << (u32::from(self.entry_format & Self::INNER_INDEX_BIT_COUNT_MASK) + 1)) - 1))
            as u16;

        Ok(DeltaSetIndexMapEntry {
            outer_index,
            inner_index,
        })
    }

    /// The size of an entry in bytes
    fn entry_size(&self) -> u8 {
        Self::entry_size_impl(self.entry_format)
    }

    fn entry_size_impl(entry_format: u8) -> u8 {
        (entry_format & Self::MAP_ENTRY_SIZE_MASK) >> 4 + 1
    }
}

impl ReadBinary for DeltaSetIndexMap<'_> {
    type HostType<'a> = DeltaSetIndexMap<'a>;

    fn read<'a>(ctxt: &mut ReadCtxt<'a>) -> Result<Self::HostType<'a>, ParseError> {
        let format = ctxt.read_u8()?;
        let entry_format = ctxt.read_u8()?;
        let map_count = match format {
            0 => ctxt.read_u16be().map(u32::from)?,
            1 => ctxt.read_u32be()?,
            _ => return Err(ParseError::BadVersion),
        };
        let entry_size = DeltaSetIndexMap::entry_size_impl(entry_format);
        let map_size = usize::from(entry_size) * usize::safe_from(map_count);
        let map_data = ctxt.read_slice(map_size)?;

        Ok(DeltaSetIndexMap {
            entry_format,
            map_count,
            map_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary::read::ReadScope;

    #[test]
    fn test_read_count() {
        let mut ctxt = ReadScope::new(&[0]).ctxt();
        assert_eq!(read_count(&mut ctxt).unwrap(), 0);
        let mut ctxt = ReadScope::new(&[0x32]).ctxt();
        assert_eq!(read_count(&mut ctxt).unwrap(), 50);
        let mut ctxt = ReadScope::new(&[0x81, 0x22]).ctxt();
        assert_eq!(read_count(&mut ctxt).unwrap(), 290);
    }

    #[test]
    fn test_read_packed_point_numbers() {
        let data = [0x0d, 0x0c, 1, 4, 4, 2, 1, 2, 3, 3, 2, 1, 1, 3, 4];
        let mut ctxt = ReadScope::new(&data).ctxt();

        let expected = vec![1, 5, 9, 11, 12, 14, 17, 20, 22, 23, 24, 27, 31];
        assert_eq!(
            read_packed_point_numbers(&mut ctxt, expected.len() as u32)
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn test_read_packed_deltas() {
        let data = [
            0x03, 0x0A, 0x97, 0x00, 0xC6, 0x87, 0x41, 0x10, 0x22, 0xFB, 0x34,
        ];
        let mut ctxt = ReadScope::new(&data).ctxt();
        let expected = vec![10, -105, 0, -58, 0, 0, 0, 0, 0, 0, 0, 0, 4130, -1228];
        assert_eq!(
            read_packed_deltas(&mut ctxt, expected.len() as u32).unwrap(),
            expected
        );
    }
}
