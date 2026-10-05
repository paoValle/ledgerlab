//! Money: exact, signed, in micro-units.
//!
//! One rule, and everything else here is a consequence: **no floats, ever**. A ledger that
//! stores `0.1 + 0.2` as `0.30000000000000004` is not a ledger, it is a rounding error with a
//! transaction history attached. Amounts are `i64` micro-units (`1 USD = 1_000_000 µUSD`), the
//! same unit `llmgateway` meters in, and the only place a decimal appears is when a human
//! reads or writes one.
//!
//! The second rule: parsing is **strict**, and it refuses what it cannot represent. `0.0000001`
//! is not "0.00", it is an amount this type cannot hold, and silently rounding it away would be
//! the ledger lying about a cent.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Micro-units per unit of currency.
pub const MICRO: i64 = 1_000_000;

/// A signed amount of money in micro-units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Amount(i64);

impl Amount {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// From micro-units, exactly.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }

    /// Builds an amount from micro-units.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Addition that refuses to wrap around.
    ///
    /// `i64` micro-units overflow at about 9.2 trillion: unreachable for a person, routine for
    /// a bug that applies a transaction twice with a wrong sign.
    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    /// The opposite amount.
    #[must_use]
    pub const fn negated(self) -> Self {
        Self(-self.0)
    }

    /// Whether the amount is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Whether the amount is negative.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// The canonical decimal form: `12.340000`, always six decimals, always a sign when negative.
    #[must_use]
    pub fn to_decimal(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let absolute = self.0.unsigned_abs();
        format!(
            "{sign}{}.{:06}",
            absolute / MICRO as u64,
            absolute % MICRO as u64
        )
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_decimal())
    }
}

impl FromStr for Amount {
    type Err = MoneyError;

    /// Parses `-12.34`, `1`, `0.000001`. Refuses more precision than it can hold, and refuses
    /// anything that is not a decimal number: a currency symbol is not an amount.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if text.is_empty() {
            return Err(MoneyError::Empty);
        }
        let (sign, digits) = match text.strip_prefix('-') {
            Some(rest) => (-1_i64, rest),
            None => (1_i64, text.strip_prefix('+').unwrap_or(text)),
        };
        let (whole, fraction) = match digits.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (digits, ""),
        };
        if whole.is_empty() && fraction.is_empty() {
            return Err(MoneyError::NotANumber(text.to_owned()));
        }
        if !whole.chars().all(|c| c.is_ascii_digit())
            || !fraction.chars().all(|c| c.is_ascii_digit())
        {
            return Err(MoneyError::NotANumber(text.to_owned()));
        }
        if fraction.len() > 6 {
            return Err(MoneyError::TooPrecise(text.to_owned()));
        }

        let whole: i64 = whole
            .parse()
            .map_err(|_| MoneyError::OutOfRange(text.to_owned()))?;
        let padding = 6 - fraction.len();
        let fraction: i64 = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<i64>()
                .map_err(|_| MoneyError::OutOfRange(text.to_owned()))?
                * 10_i64.pow(
                    u32::try_from(padding).map_err(|_| MoneyError::OutOfRange(text.to_owned()))?,
                )
        };

        let total = whole
            .checked_mul(MICRO)
            .and_then(|scaled| scaled.checked_add(fraction))
            .ok_or_else(|| MoneyError::OutOfRange(text.to_owned()))?;
        Ok(Self(sign * total))
    }
}

impl fmt::Display for MoneyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("an empty string is not an amount"),
            Self::NotANumber(text) => write!(f, "{text:?} is not a decimal number"),
            Self::TooPrecise(text) => write!(
                f,
                "{text:?} has more than six decimals: this ledger cannot represent it, and rounding it away would be a lie"
            ),
            Self::OutOfRange(text) => write!(f, "{text:?} does not fit in 64 bits of micro-units"),
            Self::Overflow => f.write_str("the sum overflowed: that is a bug, not a big number"),
        }
    }
}

impl std::error::Error for MoneyError {}

/// What can go wrong with money.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoneyError {
    /// The text was empty.
    Empty,
    /// The text was not a decimal number.
    NotANumber(String),
    /// The text had more precision than a micro-unit.
    TooPrecise(String),
    /// The number was too large.
    OutOfRange(String),
    /// A sum wrapped around.
    Overflow,
}

// Amounts travel as strings in JSON: `"12.340000"`. A number would be a float in too many
// readers, and a float here is the bug this whole module exists to prevent.
impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_decimal())
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_str(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_is_exact_and_round_trips() {
        for (text, micros) in [
            ("0", 0),
            ("1", MICRO),
            ("-1", -MICRO),
            ("12.34", 12_340_000),
            ("0.000001", 1),
            ("-0.000001", -1),
            ("+3.5", 3_500_000),
        ] {
            let amount = Amount::from_str(text).expect(text);
            assert_eq!(amount.micros(), micros, "{text}");
            assert_eq!(
                amount.to_decimal(),
                if micros < 0 {
                    format!("-{}", Amount::from_micros(-micros).to_decimal())
                } else {
                    Amount::from_micros(micros).to_decimal()
                }
            );
        }
    }

    #[test]
    fn it_refuses_what_it_cannot_represent_instead_of_rounding() {
        assert_eq!(
            Amount::from_str("0.0000001"),
            Err(MoneyError::TooPrecise("0.0000001".to_owned()))
        );
        assert_eq!(
            Amount::from_str("1.2.3"),
            Err(MoneyError::NotANumber("1.2.3".to_owned()))
        );
        assert_eq!(
            Amount::from_str("$12"),
            Err(MoneyError::NotANumber("$12".to_owned()))
        );
        assert_eq!(Amount::from_str(""), Err(MoneyError::Empty));
    }

    #[test]
    fn addition_refuses_to_wrap_around() {
        assert_eq!(
            Amount::from_micros(i64::MAX).checked_add(Amount::from_micros(1)),
            Err(MoneyError::Overflow)
        );
        assert_eq!(
            Amount::from_micros(1).checked_add(Amount::from_micros(2)),
            Ok(Amount::from_micros(3))
        );
    }
}
