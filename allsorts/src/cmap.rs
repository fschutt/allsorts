//! Implements the [cmap](cmap specification) table
//!
//! [cmap specification]: https://www.microsoft.com/typography/otspec/cmap.htm

use std::{fmt, mem};
use std::marker::PhantomData;

/// A table that defines the mappings of achacter codes to the glyph indices used in the font.
///
/// Multiple encoding schemes may be supported via the `encoding_records`.
#[repr(C)]
pub struct CMap {
    version: u16,
    num_tables: u16,
    encoding_records: [u8],
}

impl CMap {
    pub fn from_buf(buf: &[u8]) -> Result<&CMap, ()> {
        if buf.len() < CMap::min_size_of() {
            Err(())
        } else {
            let cmap = unsafe { mem::transmute::<_, &CMap>(buf) };

            if cmap.version() != 0 {
                Err(())
            } else if buf.len() != cmap.size_of() {
                Err(())
            } else {
                Ok(cmap)
            }
        }
    }

    pub fn min_size_of() -> usize {
        mem::size_of::<u16>() + // version
        mem::size_of::<u16>() + // num_tables
        0 // encoding_records
    }

    pub fn size_of(&self) -> usize {
        mem::size_of::<u16>() + // version
        mem::size_of::<u16>() + // num_tables
        mem::size_of::<EncodingRecord>() * self.num_tables() as usize // encoding_records
    }

    pub fn version(&self) -> u16 {
        u16::from_be(self.version)
    }

    pub fn num_tables(&self) -> u16 {
        u16::from_be(self.num_tables)
    }

    pub fn encoding_records(&self) -> EncodingRecords {
        EncodingRecords {
            len: self.num_tables(),
            current: 0,
            data: self.encoding_records.as_ptr(),
            _marker: PhantomData,
        }
    }
}

impl fmt::Debug for CMap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "CMap {{ version: {:?}, num_tables: {:?}, encoding_records: [..] }}",
            self.version(),
            self.num_tables()
        )
    }
}

pub struct EncodingRecords<'a> {
    len: u16,
    current: u16,
    data: *const u8,
    _marker: PhantomData<&'a ()>,
}

impl<'a> Iterator for EncodingRecords<'a> {
    type Item = Result<&'a EncodingRecord, ()>;

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len as usize, Some(self.len as usize))
    }

    fn next(&mut self) -> Option<Result<&'a EncodingRecord, ()>> {
        if self.current < self.len {
            self.current += 1;
            unsafe {
                let encoding_record = mem::transmute::<_, &EncodingRecord>(self.data);
                self.data = self.data.offset(encoding_record.size_of() as isize);
                return Some(Ok(encoding_record));
            }
        }

        None
    }
}

#[derive(PartialEq, Eq)]
#[repr(C)]
pub struct EncodingRecord {
    platform_id: u16,
    encoding_id: u16,
    subtable_offset: u32,
}

impl EncodingRecord {
    pub fn size_of(&self) -> usize {
        mem::size_of::<u16>() + // platform_id
        mem::size_of::<u16>() + // encoding_id
        mem::size_of::<u32>() // subtable_offset
    }

    pub fn platform_id(&self) -> u16 {
        u16::from_be(self.platform_id)
    }

    pub fn encoding_id(&self) -> u16 {
        u16::from_be(self.encoding_id)
    }

    pub fn subtable_offset(&self) -> u32 {
        u32::from_be(self.subtable_offset)
    }
}

impl fmt::Debug for EncodingRecord {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(
            f,
            "EncodingRecord {{ platform_id: {:?}, encoding_id: {:?}, subtable_offset: {:?} }}",
            self.platform_id(),
            self.encoding_id(),
            self.subtable_offset(),
        )
    }
}

#[cfg(test)]
mod test {
    use byteorder::{BigEndian, WriteBytesExt};

    use super::*;

    #[test]
    fn empty_data() {
        let data = Vec::new();

        assert!(CMap::from_buf(&data).is_err());
    }

    #[test]
    fn missing_length() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version

        assert!(CMap::from_buf(&data).is_err());
    }

    #[test]
    fn invalid_version() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(1).unwrap(); // version
        data.write_u16::<BigEndian>(0).unwrap(); // num_tables

        assert!(CMap::from_buf(&data).is_err());
    }

    #[test]
    fn empty_subtables() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version
        data.write_u16::<BigEndian>(0).unwrap(); // num_tables

        let cmap = CMap::from_buf(&data).unwrap();
        assert_eq!(cmap.version(), 0);
        assert_eq!(cmap.num_tables(), 0);
        assert_eq!(cmap.encoding_records().next(), None);
    }

    #[test]
    fn one_encoding_record() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version
        data.write_u16::<BigEndian>(1).unwrap(); // num_tables
        // encoding_record 0
        data.write_u16::<BigEndian>(3).unwrap(); // platform_id
        data.write_u16::<BigEndian>(10).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(256).unwrap(); // subtable_offset

        let cmap = CMap::from_buf(&data).unwrap();
        assert_eq!(cmap.version(), 0);
        assert_eq!(cmap.num_tables(), 1);

        let mut encoding_records = cmap.encoding_records();
        let encoding_record0 = encoding_records.next().unwrap().unwrap();
        assert!(encoding_records.next().is_none());

        assert_eq!(encoding_record0.platform_id(), 3);
        assert_eq!(encoding_record0.encoding_id(), 10);
        assert_eq!(encoding_record0.subtable_offset(), 256);
    }

    #[test]
    fn two_encoding_records() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version
        data.write_u16::<BigEndian>(2).unwrap(); // num_tables
        // encoding_record 0
        data.write_u16::<BigEndian>(3).unwrap(); // platform_id
        data.write_u16::<BigEndian>(10).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(256).unwrap(); // subtable_offset
        // encoding_record 1
        data.write_u16::<BigEndian>(1).unwrap(); // platform_id
        data.write_u16::<BigEndian>(0).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(513).unwrap(); // subtable_offset

        let cmap = CMap::from_buf(&data).unwrap();
        assert_eq!(cmap.version(), 0);
        assert_eq!(cmap.num_tables(), 2);

        let mut encoding_records = cmap.encoding_records();
        let encoding_record0 = encoding_records.next().unwrap().unwrap();
        let encoding_record1 = encoding_records.next().unwrap().unwrap();
        assert!(encoding_records.next().is_none());

        assert_eq!(encoding_record0.platform_id(), 3);
        assert_eq!(encoding_record0.encoding_id(), 10);
        assert_eq!(encoding_record0.subtable_offset(), 256);

        assert_eq!(encoding_record1.platform_id(), 1);
        assert_eq!(encoding_record1.encoding_id(), 0);
        assert_eq!(encoding_record1.subtable_offset(), 513);
    }

    #[test]
    fn length_too_large() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version
        data.write_u16::<BigEndian>(3).unwrap(); // num_tables
        // encoding_record 0
        data.write_u16::<BigEndian>(3).unwrap(); // platform_id
        data.write_u16::<BigEndian>(10).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(256).unwrap(); // subtable_offset
        // encoding_record 1
        data.write_u16::<BigEndian>(1).unwrap(); // platform_id
        data.write_u16::<BigEndian>(0).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(513).unwrap(); // subtable_offset

        assert!(CMap::from_buf(&data).is_err());
    }

    #[test]
    fn eof_in_encoding_record() {
        let mut data = Vec::new();

        data.write_u16::<BigEndian>(0).unwrap(); // version
        data.write_u16::<BigEndian>(3).unwrap(); // num_tables
        // encoding_record 0
        data.write_u16::<BigEndian>(3).unwrap(); // platform_id
        data.write_u16::<BigEndian>(10).unwrap(); // encoding_id
        data.write_u32::<BigEndian>(256).unwrap(); // subtable_offset
        // encoding_record 1
        data.write_u16::<BigEndian>(1).unwrap(); // platform_id
        data.write_u16::<BigEndian>(0).unwrap(); // encoding_id
        data.write_u16::<BigEndian>(0).unwrap(); // subtable_offset

        assert!(CMap::from_buf(&data).is_err());
    }
}
