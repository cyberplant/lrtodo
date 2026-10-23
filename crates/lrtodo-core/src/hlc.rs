use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Hybrid logical clock timestamp. Totally ordered by (ms, counter, node).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    pub ms: u64,
    pub counter: u32,
    pub node: String,
}

impl Hlc {
    pub fn new(ms: u64, counter: u32, node: impl Into<String>) -> Self {
        Self {
            ms,
            counter,
            node: node.into(),
        }
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:013}-{:06}-{}", self.ms, self.counter, self.node)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("HLC inválido: {0}")]
pub struct ParseHlcError(String);

impl FromStr for Hlc {
    type Err = ParseHlcError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseHlcError(s.to_string());
        let mut parts = s.splitn(3, '-');
        let ms = parts.next().and_then(|p| p.parse().ok()).ok_or_else(err)?;
        let counter = parts.next().and_then(|p| p.parse().ok()).ok_or_else(err)?;
        let node = parts.next().ok_or_else(err)?;
        Ok(Self::new(ms, counter, node))
    }
}

impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Generador de timestamps HLC para un nodo (dispositivo o servidor).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clock {
    pub node: String,
    #[serde(default)]
    last_ms: u64,
    #[serde(default)]
    last_counter: u32,
}

impl Clock {
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            last_ms: 0,
            last_counter: 0,
        }
    }

    /// Nuevo timestamp, estrictamente mayor que cualquiera emitido u observado.
    pub fn tick(&mut self, now_ms: u64) -> Hlc {
        if now_ms > self.last_ms {
            self.last_ms = now_ms;
            self.last_counter = 0;
        } else {
            self.last_counter += 1;
        }
        Hlc::new(self.last_ms, self.last_counter, self.node.clone())
    }

    /// Último HLC emitido u observado, sin avanzar el reloj.
    pub fn peek(&self) -> Hlc {
        Hlc::new(self.last_ms, self.last_counter, self.node.clone())
    }

    /// Incorpora un timestamp remoto para que los siguientes `tick` lo superen.
    pub fn observe(&mut self, h: &Hlc) {
        if (h.ms, h.counter) > (self.last_ms, self.last_counter) {
            self.last_ms = h.ms;
            self.last_counter = h.counter;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_order() {
        let a = Hlc::new(1700000000000, 3, "phone-1");
        let s = a.to_string();
        assert_eq!(s.parse::<Hlc>().unwrap(), a);
        let b = Hlc::new(1700000000000, 4, "a");
        assert!(b > a);
        assert!(s < b.to_string());
    }

    #[test]
    fn tick_monotonic_with_clock_skew() {
        let mut c = Clock::new("n");
        let a = c.tick(100);
        let b = c.tick(50);
        assert!(b > a);
        c.observe(&Hlc::new(1000, 7, "x"));
        let d = c.tick(10);
        assert!(d > Hlc::new(1000, 7, "x"));
    }
}
