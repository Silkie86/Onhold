//! Reference exchange rates from the European Central Bank: published each
//! working day around 16:00 CET, free, no account needed. The feed gives
//! how many units of each currency one euro buys; the core works out cross
//! rates from those.

use std::time::Duration;

use chrono::NaiveDate;

/// Today's rates.
pub const ECB_DAILY: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";
/// The last 90 days, for a first fetch or after a gap.
pub const ECB_90_DAYS: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-hist-90d.xml";

/// One day's rates: (currency, units per euro as a decimal string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRates {
    pub date: NaiveDate,
    pub rates: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum FxError {
    #[error("the rates could not be fetched: {0}")]
    Unavailable(String),
    #[error("the rates feed was not understood: {0}")]
    Invalid(String),
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    for quote in ['\'', '"'] {
        let key = format!("{name}={quote}");
        if let Some(start) = tag.find(&key) {
            let rest = &tag[start + key.len()..];
            return rest.find(quote).map(|end| &rest[..end]);
        }
    }
    None
}

/// Reads the ECB's XML: `<Cube time="…">` days holding
/// `<Cube currency="USD" rate="1.0812"/>` rates.
pub fn parse_ecb(xml: &str) -> Result<Vec<DayRates>, FxError> {
    let mut days: Vec<DayRates> = Vec::new();
    for tag in xml.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if !tag.starts_with("Cube") {
            continue;
        }
        if let Some(time) = attribute(tag, "time") {
            let date = NaiveDate::parse_from_str(time, "%Y-%m-%d")
                .map_err(|_| FxError::Invalid(format!("`{time}` is not a date")))?;
            days.push(DayRates {
                date,
                rates: Vec::new(),
            });
        } else if let (Some(currency), Some(rate)) =
            (attribute(tag, "currency"), attribute(tag, "rate"))
        {
            let day = days
                .last_mut()
                .ok_or_else(|| FxError::Invalid("a rate came before its date".into()))?;
            if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(FxError::Invalid(format!("`{currency}` is not a currency")));
            }
            day.rates.push((currency.to_owned(), rate.to_owned()));
        }
    }
    if days.is_empty() {
        return Err(FxError::Invalid("no dated rates were found".into()));
    }
    Ok(days)
}

/// Fetches and parses one of the ECB's feeds.
pub async fn fetch_ecb(url: &str) -> Result<Vec<DayRates>, FxError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| FxError::Unavailable(e.to_string()))?;
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| FxError::Unavailable(e.to_string()))?;
    if !res.status().is_success() {
        return Err(FxError::Unavailable(format!("{}", res.status())));
    }
    let body = res
        .text()
        .await
        .map_err(|e| FxError::Unavailable(e.to_string()))?;
    parse_ecb(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the ECB publishes it (two days, a few currencies).
    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01" xmlns="http://www.ecb.int/vocabulary/2002-08-01/eurofxref">
	<gesmes:subject>Reference rates</gesmes:subject>
	<gesmes:Sender><gesmes:name>European Central Bank</gesmes:name></gesmes:Sender>
	<Cube>
		<Cube time='2026-10-26'>
			<Cube currency='USD' rate='1.1612'/>
			<Cube currency='JPY' rate='176.42'/>
			<Cube currency='GBP' rate='0.86915'/>
		</Cube>
		<Cube time='2026-10-23'>
			<Cube currency='USD' rate='1.1598'/>
			<Cube currency='GBP' rate="0.8701"/>
		</Cube>
	</Cube>
</gesmes:Envelope>"#;

    #[test]
    fn reads_the_ecb_feed() {
        let days = parse_ecb(FEED).expect("parses");
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].date, NaiveDate::from_ymd_opt(2026, 10, 26).expect("date"));
        assert_eq!(days[0].rates[2], ("GBP".into(), "0.86915".into()));
        assert_eq!(days[1].rates[1], ("GBP".into(), "0.8701".into()));
        assert!(parse_ecb("<html>maintenance</html>").is_err());
        assert!(parse_ecb("<Cube currency='USD' rate='1'/>").is_err());
    }
}
