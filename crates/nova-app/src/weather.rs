//! Weather announcements through Open-Meteo with fixed German sentence templates.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::commands::{Day, Reply};
use crate::tools;

/// A place resolved by the geocoding API.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Place {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// Forecast values for one day.
#[derive(Debug, Clone, PartialEq)]
struct DayForecast {
    code: u8,
    min: f64,
    max: f64,
    rain_probability: Option<f64>,
}

/// Current conditions plus today and tomorrow.
#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    current_temperature: f64,
    current_code: u8,
    days: [DayForecast; 2],
}

#[derive(Deserialize)]
struct PlaceResponse {
    results: Option<Vec<Place>>,
}

#[derive(Deserialize)]
struct ForecastResponse {
    current: CurrentResponse,
    daily: DailyResponse,
}

#[derive(Deserialize)]
struct CurrentResponse {
    temperature_2m: f64,
    weather_code: u8,
}

#[derive(Deserialize)]
struct DailyResponse {
    weather_code: Vec<u8>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<f64>>,
}

/// Parse a geocoding response; `None` when the API found no place.
pub fn parse_place(json: &str) -> Result<Option<Place>, serde_json::Error> {
    let response: PlaceResponse = serde_json::from_str(json)?;
    Ok(response
        .results
        .and_then(|results| results.into_iter().next()))
}

/// Parse a forecast response covering at least today and tomorrow.
pub fn parse_forecast(json: &str) -> Result<Forecast, serde_json::Error> {
    use serde::de::Error;
    let response: ForecastResponse = serde_json::from_str(json)?;
    let daily = response.daily;
    let day = |i: usize| -> Option<DayForecast> {
        Some(DayForecast {
            code: *daily.weather_code.get(i)?,
            min: *daily.temperature_2m_min.get(i)?,
            max: *daily.temperature_2m_max.get(i)?,
            rain_probability: *daily.precipitation_probability_max.get(i)?,
        })
    };
    match (day(0), day(1)) {
        (Some(today), Some(tomorrow)) => Ok(Forecast {
            current_temperature: response.current.temperature_2m,
            current_code: response.current.weather_code,
            days: [today, tomorrow],
        }),
        _ => Err(Error::custom("daily forecast has fewer than two days")),
    }
}

/// German description of a WMO weather code.
pub fn condition(code: u8) -> &'static str {
    match code {
        0 => "klar",
        1 => "überwiegend klar",
        2 => "teils bewölkt",
        3 => "bedeckt",
        45 | 48 => "neblig",
        51 | 53 | 55 | 56 | 57 => "Nieselregen",
        61 | 63 => "Regen",
        65 => "starker Regen",
        66 | 67 => "gefrierender Regen",
        71 | 73 | 77 => "Schneefall",
        75 => "starker Schneefall",
        80..=82 => "Regenschauer",
        85 | 86 => "Schneeschauer",
        95 | 96 | 97 | 99 => "Gewitter",
        _ => UNKNOWN_CONDITION,
    }
}

const UNKNOWN_CONDITION: &str = "wechselhaft";

/// Whether the current condition is spoken as "gibt es" (precipitation or storm) instead of "ist es".
fn is_event(code: u8) -> bool {
    code >= 51 && condition(code) != UNKNOWN_CONDITION
}

/// Round to whole degrees and speak the sign as "minus"; a value rounding to 0 has no sign.
fn degrees(value: f64) -> String {
    let rounded = value.round() as i64;
    if rounded < 0 {
        format!("minus {}", -rounded)
    } else {
        rounded.to_string()
    }
}

/// Fixed German sentence for `day` in `place`.
pub fn sentence(place: &str, day: Day, forecast: &Forecast) -> String {
    let rain = |day: &DayForecast| {
        day.rain_probability
            .map(|p| format!(", Regenwahrscheinlichkeit {} Prozent", p.round() as i64))
            .unwrap_or_default()
    };
    match day {
        Day::Today => {
            let today = &forecast.days[0];
            let code = forecast.current_code;
            let verb = if is_event(code) { "gibt es" } else { "ist es" };
            format!(
                "In {place} {verb} gerade {} bei {} Grad. Heute {} bis {} Grad{}.",
                condition(code),
                degrees(forecast.current_temperature),
                degrees(today.min),
                degrees(today.max),
                rain(today),
            )
        }
        Day::Tomorrow => {
            let tomorrow = &forecast.days[1];
            format!(
                "Morgen in {place}: {}, {} bis {} Grad{}.",
                condition(tomorrow.code),
                degrees(tomorrow.min),
                degrees(tomorrow.max),
                rain(tomorrow),
            )
        }
    }
}

const UNREACHABLE: &str = "Der Wetterdienst ist nicht erreichbar.";

/// GET `url` and return the body of a 200 response.
fn fetch(url: &str) -> Option<String> {
    let mut response = tools::http_agent()
        .get(url)
        .call()
        .inspect_err(|err| tracing::warn!(%err, "weather request failed"))
        .ok()?;
    if response.status() != 200 {
        tracing::warn!(status = %response.status(), "weather service returned an error");
        return None;
    }
    response
        .body_mut()
        .read_to_string()
        .inspect_err(|err| tracing::warn!(%err, "weather response unreadable"))
        .ok()
}

/// Announce the weather for `place`, or `home` when no place was spoken.
pub fn report(place: Option<&str>, day: Day, home: &str) -> Reply {
    let wanted = place
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(home.trim());
    if wanted.is_empty() {
        return Reply::Say("Dein Heimatort fehlt in den Einstellungen.".into());
    }
    let unreachable = || Reply::Say(UNREACHABLE.into());
    let encoded = utf8_percent_encode(wanted, NON_ALPHANUMERIC);
    let Some(body) = fetch(&format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={encoded}&count=1&language=de"
    )) else {
        return unreachable();
    };
    let found = match parse_place(&body) {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(%err, "geocoding response invalid");
            return unreachable();
        }
    };
    let Some(found) = found else {
        return Reply::Say(format!("Den Ort {wanted} kenne ich nicht."));
    };
    let Some(body) = fetch(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&current=temperature_2m,weather_code&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&timezone=auto&forecast_days=2",
        found.latitude, found.longitude
    )) else {
        return unreachable();
    };
    match parse_forecast(&body) {
        Ok(forecast) => Reply::Say(sentence(&found.name, day, &forecast)),
        Err(err) => {
            tracing::warn!(%err, "forecast response invalid");
            unreachable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Day::{Today, Tomorrow};

    const FORECAST: &str = r#"{"current":{"temperature_2m":5.7,"weather_code":0},"daily":{
      "weather_code":[0,61],"temperature_2m_max":[12.9,8.2],"temperature_2m_min":[4.8,-1.6],
      "precipitation_probability_max":[0,70]}}"#;

    fn forecast(json: &str) -> Forecast {
        parse_forecast(json).expect("valid forecast")
    }

    #[test]
    fn announces_today() {
        assert_eq!(
            sentence("Köln", Today, &forecast(FORECAST)),
            "In Köln ist es gerade klar bei 6 Grad. Heute 5 bis 13 Grad, Regenwahrscheinlichkeit 0 Prozent."
        );
    }

    #[test]
    fn announces_tomorrow_with_minus() {
        assert_eq!(
            sentence("Köln", Tomorrow, &forecast(FORECAST)),
            "Morgen in Köln: Regen, minus 2 bis 8 Grad, Regenwahrscheinlichkeit 70 Prozent."
        );
    }

    #[test]
    fn precipitation_code_uses_gibt_es_template() {
        let json = FORECAST.replace(r#""weather_code":0}"#, r#""weather_code":61}"#);
        assert!(
            sentence("Köln", Today, &forecast(&json))
                .starts_with("In Köln gibt es gerade Regen bei ")
        );
    }

    #[test]
    fn unknown_code_below_51_uses_ist_es_template() {
        let json = FORECAST.replace(r#""weather_code":0}"#, r#""weather_code":50}"#);
        assert!(
            sentence("Köln", Today, &forecast(&json))
                .starts_with("In Köln ist es gerade wechselhaft bei ")
        );
    }

    #[test]
    fn unknown_code_above_51_uses_ist_es_template() {
        let json = FORECAST.replace(r#""weather_code":0}"#, r#""weather_code":60}"#);
        assert!(
            sentence("Köln", Today, &forecast(&json))
                .starts_with("In Köln ist es gerade wechselhaft bei ")
        );
    }

    #[test]
    fn rounding_to_zero_has_no_minus() {
        let json = FORECAST.replace("5.7", "-0.4");
        assert!(sentence("Köln", Today, &forecast(&json)).contains("bei 0 Grad."));
    }

    #[test]
    fn negative_current_temperature_says_minus() {
        let json = FORECAST.replace("5.7", "-3.2");
        assert!(sentence("Köln", Today, &forecast(&json)).contains("bei minus 3 Grad."));
    }

    #[test]
    fn null_rain_probability_omits_the_clause() {
        let json = FORECAST.replace("[0,70]", "[null,null]");
        for day in [Today, Tomorrow] {
            let spoken = sentence("Köln", day, &forecast(&json));
            assert!(spoken.ends_with("Grad."), "{spoken}");
            assert!(!spoken.contains("Regenwahrscheinlichkeit"), "{spoken}");
        }
    }

    #[test]
    fn short_daily_arrays_are_an_error() {
        let json = FORECAST.replace("[0,61]", "[0]");
        assert!(parse_forecast(&json).is_err());
    }

    #[test]
    fn maps_every_spec_row() {
        let rows: [(u8, &str); 17] = [
            (0, "klar"),
            (1, "überwiegend klar"),
            (2, "teils bewölkt"),
            (3, "bedeckt"),
            (45, "neblig"),
            (48, "neblig"),
            (53, "Nieselregen"),
            (63, "Regen"),
            (65, "starker Regen"),
            (66, "gefrierender Regen"),
            (73, "Schneefall"),
            (75, "starker Schneefall"),
            (81, "Regenschauer"),
            (86, "Schneeschauer"),
            (95, "Gewitter"),
            (99, "Gewitter"),
            (50, "wechselhaft"),
        ];
        for (code, expected) in rows {
            assert_eq!(condition(code), expected, "code {code}");
        }
    }

    #[test]
    fn missing_results_means_unknown_place() {
        assert_eq!(parse_place(r#"{"generationtime_ms":0.08}"#).unwrap(), None);
    }

    #[test]
    fn parses_place_hit() {
        let json = r#"{"results":[{"id":2886242,"name":"Köln","latitude":50.93333,"longitude":6.95,"country":"Deutschland"}],"generationtime_ms":0.3}"#;
        assert_eq!(
            parse_place(json).unwrap(),
            Some(Place {
                name: "Köln".into(),
                latitude: 50.93333,
                longitude: 6.95
            })
        );
    }

    #[test]
    fn missing_home_is_spoken_without_network() {
        assert_eq!(
            report(None, Today, ""),
            Reply::Say("Dein Heimatort fehlt in den Einstellungen.".into())
        );
    }
}
