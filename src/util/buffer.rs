//! Binary buffer utilities for byte-level operations
//!
//! Supports both Little-Endian and Big-Endian reads/writes
//! with buffer overflow protection.

use crate::Result;

/// Binary buffer for reading and writing data
///
/// Supports operations on:
/// - Single bytes
/// - 16-bit, 32-bit, 64-bit integers (Big/Little Endian)
/// - 24-bit (triads)
/// - VarInt (variable-length integers)
/// - Arbitrary byte sequences
#[derive(Debug, Clone)]
pub struct Buffer {
    data: Vec<u8>,
    position: usize,
}

impl Buffer {
    /// Create a new empty buffer
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            position: 0,
        }
    }

    /// Create a buffer from existing data
    pub fn from(data: &[u8]) -> Self {
        Self {
            data: data.to_vec(),
            position: 0,
        }
    }

    /// Check if buffer has remaining data to read
    pub fn has_remaining(&self) -> bool {
        self.position < self.data.len()
    }

    /// Get remaining bytes count
    pub fn remaining(&self) -> usize {
        if self.position < self.data.len() {
            self.data.len() - self.position
        } else {
            0
        }
    }

    /// Read a single byte (unsigned)
    pub fn read_u8(&mut self) -> Result<u8> {
        if self.position < self.data.len() {
            let val = self.data[self.position];
            self.position += 1;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read a signed byte
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    /// Read 16-bit unsigned integer (Big Endian)
    pub fn read_u16(&mut self) -> Result<u16> {
        if self.position + 2 <= self.data.len() {
            let val =
                ((self.data[self.position] as u16) << 8) | (self.data[self.position + 1] as u16);
            self.position += 2;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 16-bit unsigned integer (Little Endian)
    pub fn read_u16_le(&mut self) -> Result<u16> {
        if self.position + 2 <= self.data.len() {
            let val =
                (self.data[self.position] as u16) | ((self.data[self.position + 1] as u16) << 8);
            self.position += 2;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 32-bit unsigned integer (Big Endian)
    pub fn read_u32(&mut self) -> Result<u32> {
        if self.position + 4 <= self.data.len() {
            let val = ((self.data[self.position] as u32) << 24)
                | ((self.data[self.position + 1] as u32) << 16)
                | ((self.data[self.position + 2] as u32) << 8)
                | (self.data[self.position + 3] as u32);
            self.position += 4;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 32-bit unsigned integer (Little Endian)
    pub fn read_u32_le(&mut self) -> Result<u32> {
        if self.position + 4 <= self.data.len() {
            let val = (self.data[self.position] as u32)
                | ((self.data[self.position + 1] as u32) << 8)
                | ((self.data[self.position + 2] as u32) << 16)
                | ((self.data[self.position + 3] as u32) << 24);
            self.position += 4;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 24-bit unsigned integer (Little Endian) - Triad
    pub fn read_u24_le(&mut self) -> Result<u32> {
        if self.position + 3 <= self.data.len() {
            let val = (self.data[self.position] as u32)
                | ((self.data[self.position + 1] as u32) << 8)
                | ((self.data[self.position + 2] as u32) << 16);
            self.position += 3;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 64-bit unsigned integer (Big Endian)
    pub fn read_u64(&mut self) -> Result<u64> {
        if self.position + 8 <= self.data.len() {
            let mut val = 0u64;
            for i in 0..8 {
                val = (val << 8) | (self.data[self.position + i] as u64);
            }
            self.position += 8;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read 64-bit unsigned integer (Little Endian)
    pub fn read_u64_le(&mut self) -> Result<u64> {
        if self.position + 8 <= self.data.len() {
            let mut val = 0u64;
            for i in 0..8 {
                val |= (self.data[self.position + i] as u64) << (i * 8);
            }
            self.position += 8;
            Ok(val)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read VarInt (variable length integer)
    /// Used in Bedrock protocol for compact integer representation
    pub fn read_var_int(&mut self) -> Result<u32> {
        let mut result = 0u32;
        for i in 0..5 {
            let byte = self.read_u8()?;
            result |= ((byte & 0x7F) as u32) << (7 * i);
            if (byte & 0x80) == 0 {
                return Ok(result);
            }
        }
        Err(crate::Error::InvalidData("VarInt too large".into()))
    }

    /// Read 32-bit float (Big Endian)
    pub fn read_f32(&mut self) -> Result<f32> {
        let bits = self.read_u32()?;
        Ok(f32::from_bits(bits))
    }

    /// Read 32-bit float (Little Endian)
    pub fn read_f32_le(&mut self) -> Result<f32> {
        let bits = self.read_u32_le()?;
        Ok(f32::from_bits(bits))
    }

    /// Read 64-bit float (Big Endian)
    pub fn read_f64(&mut self) -> Result<f64> {
        let bits = self.read_u64()?;
        Ok(f64::from_bits(bits))
    }

    /// Read multiple bytes (without advancing much)
    pub fn read_bytes(&mut self, count: usize) -> Result<Vec<u8>> {
        if self.position + count <= self.data.len() {
            let slice = self.data[self.position..self.position + count].to_vec();
            self.position += count;
            Ok(slice)
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    /// Read remaining bytes
    pub fn read_remaining(&mut self) -> Vec<u8> {
        let slice = self.data[self.position..].to_vec();
        self.position = self.data.len();
        slice
    }

    /// Peek a byte at offset without advancing
    pub fn peek_u8(&self, offset: usize) -> Result<u8> {
        if self.position + offset < self.data.len() {
            Ok(self.data[self.position + offset])
        } else {
            Err(crate::Error::InvalidData("Buffer overflow".into()))
        }
    }

    // ===== Write operations =====

    /// Write a single byte
    pub fn write_u8(&mut self, value: u8) -> Result<()> {
        self.data.push(value);
        Ok(())
    }

    /// Write 16-bit unsigned integer (Big Endian)
    pub fn write_u16(&mut self, value: u16) -> Result<()> {
        self.data.push(((value >> 8) & 0xFF) as u8);
        self.data.push((value & 0xFF) as u8);
        Ok(())
    }

    /// Write 16-bit unsigned integer (Little Endian)
    pub fn write_u16_le(&mut self, value: u16) -> Result<()> {
        self.data.push((value & 0xFF) as u8);
        self.data.push(((value >> 8) & 0xFF) as u8);
        Ok(())
    }

    /// Write 32-bit unsigned integer (Big Endian)
    pub fn write_u32(&mut self, value: u32) -> Result<()> {
        self.data.push(((value >> 24) & 0xFF) as u8);
        self.data.push(((value >> 16) & 0xFF) as u8);
        self.data.push(((value >> 8) & 0xFF) as u8);
        self.data.push((value & 0xFF) as u8);
        Ok(())
    }

    /// Write 32-bit unsigned integer (Little Endian)
    pub fn write_u32_le(&mut self, value: u32) -> Result<()> {
        self.data.push((value & 0xFF) as u8);
        self.data.push(((value >> 8) & 0xFF) as u8);
        self.data.push(((value >> 16) & 0xFF) as u8);
        self.data.push(((value >> 24) & 0xFF) as u8);
        Ok(())
    }

    /// Write 24-bit unsigned integer (Little Endian) - Triad
    pub fn write_u24_le(&mut self, value: u32) -> Result<()> {
        self.data.push((value & 0xFF) as u8);
        self.data.push(((value >> 8) & 0xFF) as u8);
        self.data.push(((value >> 16) & 0xFF) as u8);
        Ok(())
    }

    /// Write 64-bit unsigned integer (Big Endian)
    pub fn write_u64(&mut self, value: u64) -> Result<()> {
        for i in (0..8).rev() {
            self.data.push(((value >> (i * 8)) & 0xFF) as u8);
        }
        Ok(())
    }

    /// Write 64-bit unsigned integer (Little Endian)
    pub fn write_u64_le(&mut self, value: u64) -> Result<()> {
        for i in 0..8 {
            self.data.push(((value >> (i * 8)) & 0xFF) as u8);
        }
        Ok(())
    }

    /// Write VarInt (variable length integer)
    pub fn write_var_int(&mut self, mut value: u32) -> Result<()> {
        while value > 0x7F {
            self.data.push(((value & 0x7F) | 0x80) as u8);
            value >>= 7;
        }
        self.data.push((value & 0x7F) as u8);
        Ok(())
    }

    /// Write signed VarInt (variable length integer)
    pub fn write_signed_var_int(&mut self, value: i32) -> Result<()> {
        self.write_var_int(value as u32)
    }

    /// Write a VarString (VarInt length + UTF-8 bytes)
    pub fn write_var_string(&mut self, value: &str) -> Result<()> {
        let bytes = value.as_bytes();
        self.write_var_int(bytes.len() as u32)?;
        self.write_bytes(bytes)
    }

    /// Write a boolean (single byte: 0 or 1)
    pub fn write_bool(&mut self, value: bool) -> Result<()> {
        self.write_u8(if value { 1 } else { 0 })
    }

    /// Write multiple bytes
    pub fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.data.extend_from_slice(data);
        Ok(())
    }

    /// Write 32-bit float (Big Endian)
    pub fn write_f32(&mut self, value: f32) -> Result<()> {
        self.write_u32(value.to_bits())
    }

    /// Write 32-bit float (Little Endian)
    pub fn write_f32_le(&mut self, value: f32) -> Result<()> {
        self.write_u32_le(value.to_bits())
    }

    /// Write 64-bit float (Big Endian)
    pub fn write_f64(&mut self, value: f64) -> Result<()> {
        self.write_u64(value.to_bits())
    }

    // ===== Utility methods =====

    /// Get buffer as bytes
    pub fn to_vec(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Get buffer as slice
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Reset read position to beginning
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Get current read position
    pub fn position(&self) -> usize {
        self.position
    }

    /// Set read position
    pub fn set_position(&mut self, pos: usize) {
        self.position = pos.min(self.data.len());
    }

    /// Get total buffer length
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.data.clear();
        self.position = 0;
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}
