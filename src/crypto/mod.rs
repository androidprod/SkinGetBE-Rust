//! Cryptographic operations
//!
//! This module handles:
//! - JWT processing
//! - Base64 encoding/decoding
//! - Signature verification
//! - Encryption/decryption

pub mod jwt;

pub use jwt::JWT;
