use serde::{Deserialize, Serialize};

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// PRNG determinístico (xorshift64*) para generar ULIDs sin depender de una
/// fuente de entropía del sistema: el llamador lo siembra una vez.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15 | 1,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// ULID (26 caracteres Crockford base32) para el instante `ms`.
    pub fn ulid(&mut self, ms: u64) -> String {
        let rand =
            ((self.next_u64() as u128) << 64 | self.next_u64() as u128) & ((1u128 << 80) - 1);
        encode_ulid(ms, rand)
    }
}

pub fn encode_ulid(ms: u64, rand: u128) -> String {
    let value = ((ms as u128 & ((1u128 << 48) - 1)) << 80) | rand;
    (0..26)
        .map(|i| CROCKFORD[((value >> (125 - i * 5)) & 0x1F) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_shape_and_order() {
        let mut r = Rng::new(42);
        let a = r.ulid(1_700_000_000_000);
        let b = r.ulid(1_700_000_000_001);
        assert_eq!(a.len(), 26);
        assert!(a < b);
        assert_eq!(encode_ulid(0, 0), "00000000000000000000000000");
    }
}
