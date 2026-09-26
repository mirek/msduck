//! Deterministic HASHBYTES rules and message digests, from the retained
//! SQL Server capture in `reference/hashbytes-checksum.json`.
//!
//! The digests follow RFC 1320 (MD4), RFC 1321 (MD5) and FIPS 180-4 (SHA-1,
//! SHA-256, SHA-512) and use only `std`. The caller supplies the exact bytes
//! SQL Server hashes: code-page bytes for VARCHAR/CHAR under their collation
//! (UTF-8 under a `_UTF8` collation), UTF-16LE for NVARCHAR/NCHAR, raw bytes
//! for VARBINARY, with CHAR/NCHAR padding kept. Encoding and collation lookup
//! stay with the adapter.
//!
//! CHECKSUM and BINARY_CHECKSUM are deliberately absent: see the implementation
//! section of `docs/hashbytes-checksum.md`.

/// A case this module cannot decide from captured SQL Server behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Unsupported(pub &'static str);

/// A HASHBYTES algorithm that produces a digest. MD2 resolves to NULL instead.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Algorithm {
    Md4,
    Md5,
    /// Both `SHA` and `SHA1`.
    Sha1,
    Sha2_256,
    Sha2_512,
}

impl Algorithm {
    /// DATALENGTH of the digest.
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Md4 | Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha2_256 => 32,
            Self::Sha2_512 => 64,
        }
    }

    pub fn digest(self, input: &[u8]) -> Vec<u8> {
        match self {
            Self::Md4 => md4(input).to_vec(),
            Self::Md5 => md5(input).to_vec(),
            Self::Sha1 => sha1(input).to_vec(),
            Self::Sha2_256 => sha256(input).to_vec(),
            Self::Sha2_512 => sha512(input).to_vec(),
        }
    }
}

/// Resolve the first HASHBYTES argument. `Ok(None)` is the captured NULL result
/// for MD2, unknown names (`SHA3_256`, `SHA2_384`, `''`) and a leading space.
/// ASCII letters compare case-insensitively and trailing spaces are ignored.
/// Control and non-ASCII characters were not captured and are unsupported.
pub fn resolve_algorithm(name: &str) -> Result<Option<Algorithm>, Unsupported> {
    if !name.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
        return Err(Unsupported(
            "HASHBYTES algorithm names with control or non-ASCII characters were not captured",
        ));
    }
    let name = name.trim_end_matches(' ').to_ascii_uppercase();
    Ok(match name.as_str() {
        "MD4" => Some(Algorithm::Md4),
        "MD5" => Some(Algorithm::Md5),
        "SHA" | "SHA1" => Some(Algorithm::Sha1),
        "SHA2_256" => Some(Algorithm::Sha2_256),
        "SHA2_512" => Some(Algorithm::Sha2_512),
        _ => None,
    })
}

/// Evaluate HASHBYTES over typed (bound) arguments. A NULL algorithm or input
/// is NULL, as is an unresolvable algorithm name.
pub fn hashbytes(
    algorithm: Option<&str>,
    input: Option<&[u8]>,
) -> Result<Option<Vec<u8>>, Unsupported> {
    let (Some(algorithm), Some(input)) = (algorithm, input) else {
        return Ok(None);
    };
    Ok(resolve_algorithm(algorithm)?.map(|algorithm| algorithm.digest(input)))
}

/// The static SQL type of a HASHBYTES argument. `Uncaptured` covers every type
/// whose acceptance or error text was not observed (BINARY, BIT, FLOAT, ...).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentType {
    /// The bare `NULL` literal, not a typed NULL.
    UntypedNull,
    Char,
    Varchar,
    Nchar,
    Nvarchar,
    Varbinary,
    Int,
    /// A numeric literal such as `1.5`.
    Numeric,
    Datetime,
    UniqueIdentifier,
    Xml,
    Text,
    Uncaptured,
}

impl ArgumentType {
    const fn error_name(self) -> Option<&'static str> {
        match self {
            Self::UntypedNull => Some("NULL"),
            Self::Int => Some("int"),
            Self::Numeric => Some("numeric"),
            Self::Datetime => Some("datetime"),
            Self::UniqueIdentifier => Some("uniqueidentifier"),
            Self::Xml => Some("xml"),
            Self::Text => Some("text"),
            _ => None,
        }
    }
}

/// The HASHBYTES result declaration: VARBINARY(8000), nullable, regardless of
/// algorithm or input length. TDS column flags are a root adapter concern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultType {
    pub max_length: u16,
    pub nullable: bool,
}

pub const RESULT_TYPE: ResultType = ResultType {
    max_length: 8000,
    nullable: true,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindError {
    Sql {
        number: i32,
        state: u8,
        class: u8,
        message: String,
    },
    Unsupported(Unsupported),
}

enum Check {
    Accepted,
    Rejected(&'static str),
    Unknown,
}

fn check(argument: ArgumentType, position: usize) -> Check {
    use ArgumentType::*;
    let accepted = match position {
        1 => matches!(argument, Varchar | Nvarchar),
        _ => matches!(argument, Char | Varchar | Nchar | Nvarchar | Varbinary),
    };
    if accepted {
        return Check::Accepted;
    }
    let rejected = match position {
        1 => matches!(argument, UntypedNull | Int),
        _ => argument.error_name().is_some(),
    };
    match argument.error_name() {
        Some(name) if rejected => Check::Rejected(name),
        _ => Check::Unknown,
    }
}

const UNCAPTURED_ARGUMENTS: BindError = BindError::Unsupported(Unsupported(
    "this HASHBYTES argument type combination was not captured",
));

/// Bind HASHBYTES at compile time: arity (error 174) and argument families
/// (error 8116, state 1). Errors arise before metadata, with no implicit
/// conversion of the input. Combinations whose diagnostics were not captured,
/// including two invalid arguments, are unsupported.
pub fn bind(arguments: &[ArgumentType]) -> Result<ResultType, BindError> {
    if arguments.len() != 2 {
        let captured = !arguments.is_empty()
            && arguments
                .iter()
                .all(|argument| matches!(argument, ArgumentType::Varchar));
        return Err(if captured {
            BindError::Sql {
                number: 174,
                state: 1,
                class: 15,
                message: "The hashbytes function requires 2 argument(s).".to_owned(),
            }
        } else {
            UNCAPTURED_ARGUMENTS
        });
    }
    match (check(arguments[0], 1), check(arguments[1], 2)) {
        (Check::Accepted, Check::Accepted) => Ok(RESULT_TYPE),
        (Check::Rejected(name), Check::Accepted) => Err(invalid_argument(name, 1)),
        (Check::Accepted, Check::Rejected(name)) => Err(invalid_argument(name, 2)),
        _ => Err(UNCAPTURED_ARGUMENTS),
    }
}

fn invalid_argument(type_name: &str, position: usize) -> BindError {
    BindError::Sql {
        number: 8116,
        state: 1,
        class: 16,
        message: format!(
            "Argument data type {type_name} is invalid for argument {position} of hashbytes function."
        ),
    }
}

// Message blocks with Merkle-Damgard padding: 0x80, zeros, then the message
// bit length in a `length_bytes`-wide field (little- or big-endian) ending on
// a block boundary. Complete blocks are read from `input` in place; only the
// final one or two blocks are allocated, so MAX inputs are not copied.
fn blocks<const BLOCK: usize>(
    input: &[u8],
    length_bytes: usize,
    little_endian: bool,
) -> impl Iterator<Item = [u8; BLOCK]> + '_ {
    let full = input.chunks_exact(BLOCK);
    let bits = (input.len() as u128).wrapping_mul(8);
    let mut tail = Vec::with_capacity(2 * BLOCK);
    tail.extend_from_slice(full.remainder());
    tail.push(0x80);
    while !(tail.len() + length_bytes).is_multiple_of(BLOCK) {
        tail.push(0);
    }
    if little_endian {
        tail.extend_from_slice(&bits.to_le_bytes()[..length_bytes]);
    } else {
        tail.extend_from_slice(&bits.to_be_bytes()[16 - length_bytes..]);
    }
    let tail: Vec<[u8; BLOCK]> = tail
        .chunks_exact(BLOCK)
        .map(|block| block.try_into().unwrap())
        .collect();
    full.map(|block| block.try_into().unwrap()).chain(tail)
}

fn le_words(block: &[u8]) -> [u32; 16] {
    let mut words = [0; 16];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes(bytes.try_into().unwrap());
    }
    words
}

fn le_output(state: [u32; 4]) -> [u8; 16] {
    let mut out = [0; 16];
    for (bytes, word) in out.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    out
}

const MD_INITIAL: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

/// RFC 1320 MD4.
pub fn md4(input: &[u8]) -> [u8; 16] {
    const ORDER: [[usize; 16]; 3] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15],
        [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15],
    ];
    const SHIFT: [[u32; 4]; 3] = [[3, 7, 11, 19], [3, 5, 9, 13], [3, 9, 11, 15]];
    const ADD: [u32; 3] = [0, 0x5a82_7999, 0x6ed9_eba1];
    let mut state = MD_INITIAL;
    for block in blocks::<64>(input, 8, true) {
        let x = le_words(&block);
        let [mut a, mut b, mut c, mut d] = state;
        for round in 0..3 {
            for step in 0..16 {
                let f = match round {
                    0 => (b & c) | (!b & d),
                    1 => (b & c) | (b & d) | (c & d),
                    _ => b ^ c ^ d,
                };
                let t = a
                    .wrapping_add(f)
                    .wrapping_add(x[ORDER[round][step]])
                    .wrapping_add(ADD[round])
                    .rotate_left(SHIFT[round][step % 4]);
                (a, b, c, d) = (d, t, b, c);
            }
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d]) {
            *word = word.wrapping_add(value);
        }
    }
    le_output(state)
}

/// RFC 1321 MD5.
pub fn md5(input: &[u8]) -> [u8; 16] {
    const K: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcef_a3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];
    const SHIFT: [[u32; 4]; 4] = [
        [7, 12, 17, 22],
        [5, 9, 14, 20],
        [4, 11, 16, 23],
        [6, 10, 15, 21],
    ];
    let mut state = MD_INITIAL;
    for block in blocks::<64>(input, 8, true) {
        let m = le_words(&block);
        let [mut a, mut b, mut c, mut d] = state;
        for (i, k) in K.iter().enumerate() {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let sum = a.wrapping_add(f).wrapping_add(*k).wrapping_add(m[g]);
            (a, d, c) = (d, c, b);
            b = b.wrapping_add(sum.rotate_left(SHIFT[i / 16][i % 4]));
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d]) {
            *word = word.wrapping_add(value);
        }
    }
    le_output(state)
}

/// FIPS 180-4 SHA-1.
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    for block in blocks::<64>(input, 8, false) {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for t in 16..80 {
            w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (t, word) in w.iter().enumerate() {
            let (f, k) = match t / 20 {
                0 => ((b & c) | (!b & d), 0x5a82_7999),
                1 => (b ^ c ^ d, 0x6ed9_eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, temp);
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *word = word.wrapping_add(value);
        }
    }
    let mut out = [0; 20];
    for (bytes, word) in out.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// FIPS 180-4 SHA-256.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    for block in blocks::<64>(input, 8, false) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for t in 16..64 {
            let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
            let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16]
                .wrapping_add(s0)
                .wrapping_add(w[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (k, word) in K.iter().zip(w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(value);
        }
    }
    let mut out = [0; 32];
    for (bytes, word) in out.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// FIPS 180-4 SHA-512.
pub fn sha512(input: &[u8]) -> [u8; 64] {
    const K: [u64; 80] = [
        0x428a_2f98_d728_ae22,
        0x7137_4491_23ef_65cd,
        0xb5c0_fbcf_ec4d_3b2f,
        0xe9b5_dba5_8189_dbbc,
        0x3956_c25b_f348_b538,
        0x59f1_11f1_b605_d019,
        0x923f_82a4_af19_4f9b,
        0xab1c_5ed5_da6d_8118,
        0xd807_aa98_a303_0242,
        0x1283_5b01_4570_6fbe,
        0x2431_85be_4ee4_b28c,
        0x550c_7dc3_d5ff_b4e2,
        0x72be_5d74_f27b_896f,
        0x80de_b1fe_3b16_96b1,
        0x9bdc_06a7_25c7_1235,
        0xc19b_f174_cf69_2694,
        0xe49b_69c1_9ef1_4ad2,
        0xefbe_4786_384f_25e3,
        0x0fc1_9dc6_8b8c_d5b5,
        0x240c_a1cc_77ac_9c65,
        0x2de9_2c6f_592b_0275,
        0x4a74_84aa_6ea6_e483,
        0x5cb0_a9dc_bd41_fbd4,
        0x76f9_88da_8311_53b5,
        0x983e_5152_ee66_dfab,
        0xa831_c66d_2db4_3210,
        0xb003_27c8_98fb_213f,
        0xbf59_7fc7_beef_0ee4,
        0xc6e0_0bf3_3da8_8fc2,
        0xd5a7_9147_930a_a725,
        0x06ca_6351_e003_826f,
        0x1429_2967_0a0e_6e70,
        0x27b7_0a85_46d2_2ffc,
        0x2e1b_2138_5c26_c926,
        0x4d2c_6dfc_5ac4_2aed,
        0x5338_0d13_9d95_b3df,
        0x650a_7354_8baf_63de,
        0x766a_0abb_3c77_b2a8,
        0x81c2_c92e_47ed_aee6,
        0x9272_2c85_1482_353b,
        0xa2bf_e8a1_4cf1_0364,
        0xa81a_664b_bc42_3001,
        0xc24b_8b70_d0f8_9791,
        0xc76c_51a3_0654_be30,
        0xd192_e819_d6ef_5218,
        0xd699_0624_5565_a910,
        0xf40e_3585_5771_202a,
        0x106a_a070_32bb_d1b8,
        0x19a4_c116_b8d2_d0c8,
        0x1e37_6c08_5141_ab53,
        0x2748_774c_df8e_eb99,
        0x34b0_bcb5_e19b_48a8,
        0x391c_0cb3_c5c9_5a63,
        0x4ed8_aa4a_e341_8acb,
        0x5b9c_ca4f_7763_e373,
        0x682e_6ff3_d6b2_b8a3,
        0x748f_82ee_5def_b2fc,
        0x78a5_636f_4317_2f60,
        0x84c8_7814_a1f0_ab72,
        0x8cc7_0208_1a64_39ec,
        0x90be_fffa_2363_1e28,
        0xa450_6ceb_de82_bde9,
        0xbef9_a3f7_b2c6_7915,
        0xc671_78f2_e372_532b,
        0xca27_3ece_ea26_619c,
        0xd186_b8c7_21c0_c207,
        0xeada_7dd6_cde0_eb1e,
        0xf57d_4f7f_ee6e_d178,
        0x06f0_67aa_7217_6fba,
        0x0a63_7dc5_a2c8_98a6,
        0x113f_9804_bef9_0dae,
        0x1b71_0b35_131c_471b,
        0x28db_77f5_2304_7d84,
        0x32ca_ab7b_40c7_2493,
        0x3c9e_be0a_15c9_bebc,
        0x431d_67c4_9c10_0d4c,
        0x4cc5_d4be_cb3e_42b6,
        0x597f_299c_fc65_7e2a,
        0x5fcb_6fab_3ad6_faec,
        0x6c44_198c_4a47_5817,
    ];
    let mut state: [u64; 8] = [
        0x6a09_e667_f3bc_c908,
        0xbb67_ae85_84ca_a73b,
        0x3c6e_f372_fe94_f82b,
        0xa54f_f53a_5f1d_36f1,
        0x510e_527f_ade6_82d1,
        0x9b05_688c_2b3e_6c1f,
        0x1f83_d9ab_fb41_bd6b,
        0x5be0_cd19_137e_2179,
    ];
    for block in blocks::<128>(input, 16, false) {
        let mut w = [0u64; 80];
        for (word, bytes) in w.iter_mut().zip(block.chunks_exact(8)) {
            *word = u64::from_be_bytes(bytes.try_into().unwrap());
        }
        for t in 16..80 {
            let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
            let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
            w[t] = w[t - 16]
                .wrapping_add(s0)
                .wrapping_add(w[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for (k, word) in K.iter().zip(w) {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(word);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(value);
        }
    }
    let mut out = [0; 64];
    for (bytes, word) in out.chunks_exact_mut(8).zip(state) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    out
}
