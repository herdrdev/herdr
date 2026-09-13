use serde::{Deserialize, Serialize};

use super::{SidebarTokenColor, SidebarTokenStyle};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRule", into = "RawRule")]
pub struct SidebarTokenRule {
    condition: Condition,
    ignore_case: bool,
    style: SidebarTokenStyle,
    hide: Option<bool>,
}

// Deserialization rejects non-finite thresholds, so equality is reflexive.
impl Eq for SidebarTokenRule {}

#[derive(Debug, Clone, PartialEq)]
enum Condition {
    Equals(String),
    Contains(String),
    StartsWith(String),
    GreaterThan(f64),
    LessThan(f64),
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    equals: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    starts_with: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ignore_case: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fg: Option<SidebarTokenColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dim: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hide: Option<bool>,
}

impl TryFrom<RawRule> for SidebarTokenRule {
    type Error = String;

    fn try_from(raw: RawRule) -> Result<Self, Self::Error> {
        let count = [
            raw.equals.is_some(),
            raw.contains.is_some(),
            raw.starts_with.is_some(),
            raw.gt.is_some(),
            raw.lt.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if count != 1 {
            return Err(
                "sidebar rule requires exactly one of equals, contains, starts_with, gt, lt".into(),
            );
        }
        let condition = if let Some(value) = raw.equals {
            Condition::Equals(value)
        } else if let Some(value) = raw.contains {
            Condition::Contains(value)
        } else if let Some(value) = raw.starts_with {
            Condition::StartsWith(value)
        } else {
            let (value, greater) = match (raw.gt, raw.lt) {
                (Some(value), _) => (value, true),
                (_, Some(value)) => (value, false),
                _ => unreachable!("validated condition count"),
            };
            if !value.is_finite() {
                return Err("sidebar numeric rule threshold must be finite".into());
            }
            if raw.ignore_case.is_some() {
                return Err("ignore_case applies only to sidebar text conditions".into());
            }
            if greater {
                Condition::GreaterThan(value)
            } else {
                Condition::LessThan(value)
            }
        };
        Ok(Self {
            condition,
            ignore_case: raw.ignore_case.unwrap_or(false),
            hide: raw.hide,
            style: SidebarTokenStyle {
                fg: raw.fg,
                bold: raw.bold,
                dim: raw.dim,
            },
        })
    }
}

impl From<SidebarTokenRule> for RawRule {
    fn from(rule: SidebarTokenRule) -> Self {
        let mut raw = Self {
            ignore_case: rule.ignore_case.then_some(true),
            fg: rule.style.fg,
            bold: rule.style.bold,
            dim: rule.style.dim,
            hide: rule.hide,
            ..Self::default()
        };
        match rule.condition {
            Condition::Equals(value) => raw.equals = Some(value),
            Condition::Contains(value) => raw.contains = Some(value),
            Condition::StartsWith(value) => raw.starts_with = Some(value),
            Condition::GreaterThan(value) => raw.gt = Some(value),
            Condition::LessThan(value) => raw.lt = Some(value),
        }
        raw
    }
}

impl SidebarTokenRule {
    fn matches(&self, value: &str, numeric: &mut Option<Option<f64>>) -> bool {
        match &self.condition {
            Condition::Equals(expected) => {
                if self.ignore_case {
                    value.eq_ignore_ascii_case(expected)
                } else {
                    value == expected
                }
            }
            Condition::StartsWith(expected) => {
                if self.ignore_case {
                    value
                        .as_bytes()
                        .get(..expected.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected.as_bytes()))
                } else {
                    value.starts_with(expected)
                }
            }
            Condition::Contains(expected) => {
                if self.ignore_case {
                    expected.is_empty()
                        || value
                            .as_bytes()
                            .windows(expected.len())
                            .any(|part| part.eq_ignore_ascii_case(expected.as_bytes()))
                } else {
                    value.contains(expected)
                }
            }
            Condition::GreaterThan(threshold) | Condition::LessThan(threshold) => {
                let parsed = numeric.get_or_insert_with(|| {
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|number| number.is_finite())
                });
                parsed.is_some_and(|number| match self.condition {
                    Condition::GreaterThan(_) => number > *threshold,
                    _ => number < *threshold,
                })
            }
        }
    }
}

pub(super) fn matching_style(
    rules: &[SidebarTokenRule],
    base: SidebarTokenStyle,
    value: &str,
) -> Option<SidebarTokenStyle> {
    let mut numeric = None;
    for rule in rules {
        if rule.matches(value, &mut numeric) {
            if rule.hide == Some(true) {
                return None;
            }
            return Some(SidebarTokenStyle {
                fg: rule.style.fg.or(base.fg),
                bold: rule.style.bold.or(base.bold),
                dim: rule.style.dim.or(base.dim),
            });
        }
    }
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_conditions_use_exact_case_or_ascii_folding() {
        for (condition, yes, no) in [
            ("equals = 'Local'", "Local", "Localhost"),
            ("contains = 'Local'", "myLocalbox", "remote"),
            ("starts_with = 'Local'", "Localhost", "myLocal"),
        ] {
            let rule: SidebarTokenRule = toml::from_str(condition).unwrap();
            assert!(rule.matches(yes, &mut None));
            assert!(!rule.matches(no, &mut None));
            assert!(!rule.matches(&yes.to_ascii_lowercase(), &mut None));
            let folded: SidebarTokenRule =
                toml::from_str(&format!("{condition}\nignore_case = true")).unwrap();
            assert!(folded.matches(&yes.to_ascii_lowercase(), &mut None));
        }
        for condition in ["equals", "contains", "starts_with"] {
            let rule: SidebarTokenRule =
                toml::from_str(&format!("{condition} = 'ÉA'\nignore_case = true")).unwrap();
            assert!(rule.matches("Éa", &mut None));
            assert!(!rule.matches("éa", &mut None));
        }
        let empty: SidebarTokenRule = toml::from_str("contains = ''\nignore_case = true").unwrap();
        assert!(empty.matches("", &mut None));
    }

    #[test]
    fn numeric_conditions_require_full_finite_numbers_and_strict_comparison() {
        for (condition, yes, no) in [
            ("gt = 80", ["90", "8.1e1", "+90"], "70"),
            ("lt = 80", ["70", "7.9e1", "-90"], "90"),
        ] {
            let rule: SidebarTokenRule = toml::from_str(condition).unwrap();
            for value in yes {
                assert!(rule.matches(value, &mut None), "{condition}: {value}");
            }
            for value in [
                no, "80", "90%", " 90", "90 ", "", "NaN", "inf", "-inf", "1e999",
            ] {
                assert!(!rule.matches(value, &mut None), "{condition}: {value}");
            }
        }
    }
}
