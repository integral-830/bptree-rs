use std::io;

pub const PAGE_SIZE: usize = 4096;

pub type SlotId = u16;
const HEADER_SIZE: usize = 4;
pub const SLOT_SIZE: usize = 4;

pub const MAX_AVAILABLE_SLOTTED_PAGE_SPACE: usize = PAGE_SIZE - (HEADER_SIZE);

#[derive(Debug)]
pub struct SlottedPage {
    data: [u8; PAGE_SIZE],
}

impl SlottedPage {
    /// Create and return new Instance of `SlottedPage` struct
    pub fn new() -> Self {
        let mut data = [0u8; PAGE_SIZE];
        data[2..4].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        Self { data }
    }

    /// Returns the slot counts in page
    ///
    /// # Panics
    ///
    /// Panics is not possible the struct is initialized with 0u8
    pub fn get_slot_count(&self) -> u16 {
        u16::from_le_bytes(self.data[0..2].try_into().unwrap())
    }

    /// Returns the free end of page
    ///
    /// # Panics
    ///
    /// Panics is not possible the struct is initialized with 0u8
    pub fn free_end(&self) -> u16 {
        u16::from_le_bytes(self.data[2..4].try_into().unwrap())
    }

    /// Returns the free avail space in page
    pub fn free_space(&self) -> u16 {
        let slot_count = self.get_slot_count();
        let free_end = self.free_end();
        free_end - (slot_count * SLOT_SIZE as u16 + HEADER_SIZE as u16)
    }

    /// Reads the slot and returns the `tuple_offset` and `tuple_length`
    fn read_slot(&self, slot: SlotId) -> (u16, u16) {
        let slot_offset = HEADER_SIZE + slot as usize * SLOT_SIZE;
        let tuple_offset =
            u16::from_le_bytes(self.data[slot_offset..slot_offset + 2].try_into().unwrap());
        let tuple_length = u16::from_le_bytes(
            self.data[slot_offset + 2..slot_offset + 4]
                .try_into()
                .unwrap(),
        );
        (tuple_offset, tuple_length)
    }

    /// writes the given `slot_id`
    fn write_slot(&mut self, slot: SlotId, offset: u16, length: u16) {
        let slot_offset = HEADER_SIZE + slot as usize * SLOT_SIZE;
        self.data[slot_offset..slot_offset + 2].copy_from_slice(&offset.to_le_bytes());
        self.data[slot_offset + 2..slot_offset + 4].copy_from_slice(&length.to_le_bytes());
    }

    /// Check and insert the given record into the page
    pub fn insert(&mut self, record: &[u8]) -> io::Result<SlotId> {
        let required_size = record.len() + SLOT_SIZE;
        let free_size = self.free_space() as usize;
        if free_size < required_size {
            return Err(io::Error::other("Out of memory..."));
        }
        let free_end = self.free_end() as usize;
        let tuple_offset = free_end - record.len();
        self.data[tuple_offset..free_end].copy_from_slice(record);
        self.data[2..4].copy_from_slice(&((tuple_offset as u16).to_le_bytes()));
        let slot_count = self.get_slot_count();
        self.data[0..2].copy_from_slice(&(slot_count + 1).to_le_bytes());
        self.write_slot(slot_count, tuple_offset as u16, record.len() as u16);
        Ok(slot_count)
    }

    /// Returns the borrowed data of a slot
    pub fn get(&self, slot: SlotId) -> &[u8] {
        let (tuple_offset, tuple_length) = self.read_slot(slot);
        &self.data[tuple_offset as usize..(tuple_offset + tuple_length) as usize]
    }

    /// Returns the borrowed data of a slot
    pub fn try_get(&self, slot: SlotId) -> io::Result<&[u8]> {
        let slot_count = self.get_slot_count();

        if slot >= slot_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slot ID is out of range",
            ));
        }

        let slot_dir_end = HEADER_SIZE
            .checked_add(slot_count as usize * SLOT_SIZE)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "slot directory size overflow")
            })?;

        if slot_dir_end > PAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "slot directory extends beyond page",
            ));
        }

        let (offset, length) = self.read_slot(slot);

        if length == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slot is deleted or contains an empty record",
            ));
        }

        let offset = offset as usize;
        let length = length as usize;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "record range overflow"))?;

        let free_end = self.free_end() as usize;

        if free_end < slot_dir_end
            || free_end > PAGE_SIZE
            || offset < free_end
            || offset < slot_dir_end
            || end > PAGE_SIZE
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid record range",
            ));
        }

        Ok(&self.data[offset..end])
    }

    /// Returns the borrowed bytes of a slot for serialization
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    /// Reconstructs the `SlottedPage` from raw bytes
    pub fn from_bytes(data: [u8; PAGE_SIZE]) -> io::Result<Self> {
        let slot_count = u16::from_le_bytes([data[0], data[1]]) as usize;
        let free_end = u16::from_le_bytes([data[2], data[3]]) as usize;

        let slot_dir_end = HEADER_SIZE
            .checked_add(slot_count.checked_mul(SLOT_SIZE).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "slot directory size overflow")
            })?)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "slot directory size overflow")
            })?;

        if slot_dir_end > PAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "slot directory extends beyond page",
            ));
        }

        if free_end < slot_dir_end || free_end > PAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid free_end boundary",
            ));
        }

        let mut live_ranges: Vec<(usize, usize)> = Vec::new();

        for slot in 0..slot_count {
            let slot_offset = HEADER_SIZE + slot * SLOT_SIZE;

            let offset = u16::from_le_bytes([data[slot_offset], data[slot_offset + 1]]) as usize;

            let length =
                u16::from_le_bytes([data[slot_offset + 2], data[slot_offset + 3]]) as usize;

            // Zero-length slots represent tombstones.
            if length == 0 {
                continue;
            }

            let record_end = offset.checked_add(length).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "record range overflow")
            })?;

            if offset < slot_dir_end || offset < free_end || record_end > PAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record range is outside the valid record area",
                ));
            }

            // Reject overlapping live records.
            for &(existing_start, existing_end) in &live_ranges {
                if offset < existing_end && existing_start < record_end {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "live record ranges overlap",
                    ));
                }
            }

            live_ranges.push((offset, record_end));
        }

        Ok(Self { data })
    }

    /// Deletes the given `slot_id` from the page
    pub fn delete(&mut self, slot: SlotId) -> io::Result<()> {
        let slot_count = self.get_slot_count();
        if slot >= slot_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Slot doesn't exist...",
            ));
        }
        self.write_slot(slot, 0, 0);
        Ok(())
    }

    /// Comapction helper to reclaim the deleted slots space
    pub fn compact(&mut self) {
        let slot_count = self.get_slot_count();
        let mut destination = PAGE_SIZE;
        for i in 0..slot_count {
            let (offset, length) = self.read_slot(i);
            if length == 0 {
                continue;
            }
            let mut source = vec![0u8; length as usize];
            source.copy_from_slice(&self.data[offset as usize..offset as usize + length as usize]);
            self.data[destination - length as usize..destination].copy_from_slice(&source);
            self.write_slot(i, destination as u16 - length, length);
            destination -= length as usize;
        }
        self.data[2..4].copy_from_slice(&(destination as u16).to_le_bytes());
    }
}

impl Default for SlottedPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod slotted_page_tests {
    use super::*;

    fn page() -> SlottedPage {
        SlottedPage::new()
    }

    #[test]
    fn new_page_has_zero_slots() {
        let page = page();

        assert_eq!(page.get_slot_count(), 0);
    }

    #[test]
    fn new_page_free_end_is_page_size() {
        let page = page();

        assert_eq!(page.free_end(), PAGE_SIZE as u16);
    }

    #[test]
    fn new_page_has_expected_free_space() {
        let page = page();

        assert_eq!(page.free_space(), (PAGE_SIZE - HEADER_SIZE) as u16);
    }

    #[test]
    fn default_creates_empty_page() {
        let page = SlottedPage::default();

        assert_eq!(page.get_slot_count(), 0);
        assert_eq!(page.free_end(), PAGE_SIZE as u16);
    }

    #[test]
    fn insert_returns_slot_zero_for_first_record() {
        let mut page = page();

        let slot = page.insert(b"hello").unwrap();

        assert_eq!(slot, 0);
    }

    #[test]
    fn insert_increments_slot_count() {
        let mut page = page();

        page.insert(b"first").unwrap();
        page.insert(b"second").unwrap();

        assert_eq!(page.get_slot_count(), 2);
    }

    #[test]
    fn insert_returns_sequential_slot_ids() {
        let mut page = page();

        let first = page.insert(b"one").unwrap();
        let second = page.insert(b"two").unwrap();
        let third = page.insert(b"three").unwrap();

        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert_eq!(third, 2);
    }

    #[test]
    fn get_returns_inserted_record() {
        let mut page = page();
        let record = b"hello, slotted page";

        let slot = page.insert(record).unwrap();

        assert_eq!(page.get(slot), record);
    }

    #[test]
    fn multiple_records_can_be_read_independently() {
        let mut page = page();

        let first = page.insert(b"alpha").unwrap();
        let second = page.insert(b"beta").unwrap();
        let third = page.insert(b"gamma").unwrap();

        assert_eq!(page.get(first), b"alpha");
        assert_eq!(page.get(second), b"beta");
        assert_eq!(page.get(third), b"gamma");
    }

    #[test]
    fn insert_preserves_record_bytes() {
        let mut page = page();
        let record = [0x00, 0xFF, 0x10, 0x80, 0x7F];

        let slot = page.insert(&record).unwrap();

        assert_eq!(page.get(slot), record);
    }

    #[test]
    fn insert_updates_free_end() {
        let mut page = page();

        page.insert(b"hello").unwrap();

        assert_eq!(page.free_end(), (PAGE_SIZE - 5) as u16);
    }

    #[test]
    fn insert_consumes_record_length_plus_slot_size() {
        let mut page = page();
        let initial_free_space = page.free_space();

        page.insert(b"hello").unwrap();

        assert_eq!(page.free_space(), initial_free_space - 5 - SLOT_SIZE as u16);
    }

    #[test]
    fn insert_empty_record_returns_slot_id() {
        let mut page = page();

        let result = page.insert(b"");

        assert_eq!(result.unwrap(), 0);
        assert_eq!(page.get_slot_count(), 1);
    }

    #[test]
    fn delete_existing_slot_succeeds() {
        let mut page = page();
        let slot = page.insert(b"delete me").unwrap();

        assert!(page.delete(slot).is_ok());
    }

    #[test]
    fn delete_does_not_decrement_slot_count() {
        let mut page = page();
        let slot = page.insert(b"delete me").unwrap();

        page.delete(slot).unwrap();

        assert_eq!(page.get_slot_count(), 1);
    }

    #[test]
    fn delete_invalid_slot_returns_error() {
        let mut page = page();

        let result = page.delete(0);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn delete_slot_outside_slot_count_returns_error() {
        let mut page = page();
        page.insert(b"only record").unwrap();

        let result = page.delete(1);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn delete_does_not_change_other_records() {
        let mut page = page();

        let first = page.insert(b"first").unwrap();
        let second = page.insert(b"second").unwrap();

        page.delete(first).unwrap();

        assert_eq!(page.get(second), b"second");
    }

    #[test]
    fn delete_does_not_immediately_reclaim_space() {
        let mut page = page();
        let slot = page.insert(b"some data").unwrap();
        let free_space_before_delete = page.free_space();

        page.delete(slot).unwrap();

        assert_eq!(page.free_space(), free_space_before_delete);
    }

    #[test]
    fn compact_preserves_live_records() {
        let mut page = page();

        let first = page.insert(b"first").unwrap();
        let second = page.insert(b"second").unwrap();
        let third = page.insert(b"third").unwrap();

        page.compact();

        assert_eq!(page.get(first), b"first");
        assert_eq!(page.get(second), b"second");
        assert_eq!(page.get(third), b"third");
    }

    #[test]
    fn compact_preserves_slot_count() {
        let mut page = page();

        page.insert(b"first").unwrap();
        page.insert(b"second").unwrap();
        page.delete(0).unwrap();

        page.compact();

        assert_eq!(page.get_slot_count(), 2);
    }

    #[test]
    fn compact_preserves_slot_ids_of_live_records() {
        let mut page = page();

        let first = page.insert(b"first").unwrap();
        let second = page.insert(b"second").unwrap();
        let third = page.insert(b"third").unwrap();

        page.delete(second).unwrap();
        page.compact();

        assert_eq!(page.get(first), b"first");
        assert_eq!(page.get(third), b"third");
    }

    #[test]
    fn compact_reclaims_deleted_record_space() {
        let mut page = page();

        let first = page.insert(b"first record").unwrap();
        let second = page.insert(b"second record").unwrap();

        page.delete(first).unwrap();

        let free_before_compact = page.free_space();

        page.compact();

        assert!(page.free_space() > free_before_compact);
        assert_eq!(page.get(second), b"second record");
    }

    #[test]
    fn compact_without_deletions_preserves_records() {
        let mut page = page();

        page.insert(b"alpha").unwrap();
        page.insert(b"beta").unwrap();

        let free_before = page.free_space();

        page.compact();

        assert_eq!(page.get(0), b"alpha");
        assert_eq!(page.get(1), b"beta");
        assert_eq!(page.free_space(), free_before);
    }

    #[test]
    fn compact_empty_page_is_safe() {
        let mut page = page();

        page.compact();

        assert_eq!(page.get_slot_count(), 0);
        assert_eq!(page.free_end(), PAGE_SIZE as u16);
    }

    #[test]
    fn insert_fails_when_record_exceeds_page_capacity() {
        let mut page = page();
        let record = vec![0u8; PAGE_SIZE];

        let result = page.insert(&record);

        assert!(result.is_err());
    }

    #[test]
    fn failed_insert_does_not_increment_slot_count() {
        let mut page = page();
        let record = vec![0u8; PAGE_SIZE];

        let _ = page.insert(&record);

        assert_eq!(page.get_slot_count(), 0);
    }

    #[test]
    fn failed_insert_does_not_change_free_end() {
        let mut page = page();
        let free_end_before = page.free_end();
        let record = vec![0u8; PAGE_SIZE];

        let _ = page.insert(&record);

        assert_eq!(page.free_end(), free_end_before);
    }

    #[test]
    fn insert_fills_page_until_insufficient_space() {
        let mut page = page();
        let record = vec![0xAB; 100];

        let mut successful_inserts = 0;

        while page.insert(&record).is_ok() {
            successful_inserts += 1;
        }

        assert!(successful_inserts > 0);
        assert!(page.free_space() < (record.len() + SLOT_SIZE) as u16);
    }

    #[test]
    fn free_space_never_increases_after_successful_insert() {
        let mut page = page();

        let before = page.free_space();
        page.insert(b"record").unwrap();
        let after = page.free_space();

        assert!(after < before);
    }

    #[test]
    fn from_bytes_round_trips_valid_page() {
        let mut original = SlottedPage::new();
        original.insert(b"alpha").unwrap();
        original.insert(b"beta").unwrap();

        let bytes = *original.as_bytes();
        let restored = SlottedPage::from_bytes(bytes).unwrap();

        assert_eq!(restored.get_slot_count(), 2);
        assert_eq!(restored.try_get(0).unwrap(), b"alpha");
        assert_eq!(restored.try_get(1).unwrap(), b"beta");
    }

    #[test]
    fn as_bytes_returns_fixed_size_page() {
        let page = SlottedPage::new();

        assert_eq!(page.as_bytes().len(), PAGE_SIZE);
    }

    #[test]
    fn from_bytes_accepts_new_empty_page() {
        let original = SlottedPage::new();

        let restored = SlottedPage::from_bytes(*original.as_bytes()).unwrap();

        assert_eq!(restored.get_slot_count(), 0);
        assert_eq!(restored.free_end(), PAGE_SIZE as u16);
    }

    #[test]
    fn try_get_returns_error_for_out_of_range_slot() {
        let mut page = SlottedPage::new();
        page.insert(b"record").unwrap();

        let result = page.try_get(1);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn try_get_returns_error_for_deleted_slot() {
        let mut page = SlottedPage::new();
        let slot = page.insert(b"record").unwrap();
        page.delete(slot).unwrap();

        let result = page.try_get(slot);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn from_bytes_rejects_free_end_beyond_page_size() {
        let mut page = SlottedPage::new();
        page.data[2..4].copy_from_slice(&((PAGE_SIZE as u16) + 1).to_le_bytes());

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_free_end_before_slot_directory() {
        let mut page = SlottedPage::new();
        page.insert(b"record").unwrap();

        page.data[2..4].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_slot_directory_beyond_page() {
        let mut page = SlottedPage::new();

        page.data[0..2].copy_from_slice(&u16::MAX.to_le_bytes());

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_slot_pointing_into_header() {
        let mut page = SlottedPage::new();
        page.insert(b"abc").unwrap();

        page.write_slot(0, 0, 3);

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_slot_pointing_into_slot_directory() {
        let mut page = SlottedPage::new();
        page.insert(b"abc").unwrap();

        page.write_slot(0, (HEADER_SIZE + SLOT_SIZE) as u16, 3);

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_record_range_beyond_page() {
        let mut page = SlottedPage::new();
        page.insert(b"abc").unwrap();

        page.write_slot(0, (PAGE_SIZE - 1) as u16, 10);

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_bytes_rejects_record_range_crossing_free_end() {
        let mut page = SlottedPage::new();
        page.insert(b"abc").unwrap();

        let free_end = page.free_end();
        page.write_slot(0, free_end, 4);

        let result = SlottedPage::from_bytes(page.data);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
