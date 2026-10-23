// Ninimma modification: independent project branding and integration.
//! Where a photo was taken, worked out offline: GPS position to the nearest
//! town, its state or region, and its country, from GeoNames' list of
//! towns with 1,000+ people (bundled; CC BY 4.0). Nothing is looked up
//! online.
//!
//! Also the US state abbreviations, so "Arizona" finds receipts that only
//! say "AZ", and the other way round.

use reverse_geocoder::ReverseGeocoder;

/// A place, for showing and for searching.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// "Sedona, Arizona, United States"
    pub display: String,
    /// The same, plus abbreviations ("AZ US"), for the search index.
    pub searchable: String,
}

/// The lookup table takes ~30 MB once loaded, so it's built only when a
/// photo with a position turns up, and dropped after each indexing pass.
#[derive(Default)]
pub struct Geocoder {
    inner: Option<ReverseGeocoder>,
}

impl Geocoder {
    pub fn place(&mut self, lat: f64, lon: f64) -> Option<Place> {
        if !(-90.0..=90.0).contains(&lat)
            || !(-180.0..=180.0).contains(&lon)
            || (lat == 0.0 && lon == 0.0)
        {
            return None; // missing or bogus position
        }
        let geocoder = self.inner.get_or_insert_with(ReverseGeocoder::new);
        let r = geocoder.search((lat, lon)).record;
        let country = country_name(&r.cc).unwrap_or(&r.cc);
        let mut parts: Vec<&str> = vec![r.name.as_str()];
        if !r.admin1.is_empty() && r.admin1 != r.name {
            parts.push(&r.admin1);
        }
        parts.push(country);
        let display = parts.join(", ");
        let mut searchable = format!("{display} {}", r.cc);
        if r.cc == "US" {
            if let Some(abbr) = us_state_abbreviation(&r.admin1) {
                searchable.push(' ');
                searchable.push_str(abbr);
            }
        }
        Some(Place {
            display,
            searchable,
        })
    }

    /// Frees the table (after an indexing pass).
    pub fn release(&mut self) {
        self.inner = None;
    }
}

const US_STATES: &[(&str, &str)] = &[
    ("Alabama", "AL"),
    ("Alaska", "AK"),
    ("Arizona", "AZ"),
    ("Arkansas", "AR"),
    ("California", "CA"),
    ("Colorado", "CO"),
    ("Connecticut", "CT"),
    ("Delaware", "DE"),
    ("District of Columbia", "DC"),
    ("Washington, D.C.", "DC"),
    ("Florida", "FL"),
    ("Georgia", "GA"),
    ("Hawaii", "HI"),
    ("Idaho", "ID"),
    ("Illinois", "IL"),
    ("Indiana", "IN"),
    ("Iowa", "IA"),
    ("Kansas", "KS"),
    ("Kentucky", "KY"),
    ("Louisiana", "LA"),
    ("Maine", "ME"),
    ("Maryland", "MD"),
    ("Massachusetts", "MA"),
    ("Michigan", "MI"),
    ("Minnesota", "MN"),
    ("Mississippi", "MS"),
    ("Missouri", "MO"),
    ("Montana", "MT"),
    ("Nebraska", "NE"),
    ("Nevada", "NV"),
    ("New Hampshire", "NH"),
    ("New Jersey", "NJ"),
    ("New Mexico", "NM"),
    ("New York", "NY"),
    ("North Carolina", "NC"),
    ("North Dakota", "ND"),
    ("Ohio", "OH"),
    ("Oklahoma", "OK"),
    ("Oregon", "OR"),
    ("Pennsylvania", "PA"),
    ("Rhode Island", "RI"),
    ("South Carolina", "SC"),
    ("South Dakota", "SD"),
    ("Tennessee", "TN"),
    ("Texas", "TX"),
    ("Utah", "UT"),
    ("Vermont", "VT"),
    ("Virginia", "VA"),
    ("Washington", "WA"),
    ("West Virginia", "WV"),
    ("Wisconsin", "WI"),
    ("Wyoming", "WY"),
    ("Puerto Rico", "PR"),
];

pub fn us_state_abbreviation(name: &str) -> Option<&'static str> {
    US_STATES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, a)| *a)
}

/// Other words a search word should also match: a US state's
/// abbreviation for its name, and the name for the abbreviation (only when
/// written in capitals, so "in" or "or" aren't taken for Indiana or Oregon).
pub fn synonyms(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(abbr) = us_state_abbreviation(word) {
        out.push(abbr.to_lowercase());
    }
    if word.len() == 2 && word.chars().all(|c| c.is_ascii_uppercase()) {
        if let Some((name, _)) = US_STATES.iter().find(|(_, a)| *a == word) {
            out.push(name.to_lowercase());
        }
    }
    out
}

/// ISO 3166 country names for the codes GeoNames uses.
const COUNTRIES: &str = "AD Andorra|AE United Arab Emirates|AF Afghanistan|AG Antigua and Barbuda|AI Anguilla|\
AL Albania|AM Armenia|AO Angola|AR Argentina|AS American Samoa|AT Austria|AU Australia|AW Aruba|\
AX Aland Islands|AZ Azerbaijan|BA Bosnia and Herzegovina|BB Barbados|BD Bangladesh|BE Belgium|\
BF Burkina Faso|BG Bulgaria|BH Bahrain|BI Burundi|BJ Benin|BL Saint Barthelemy|BM Bermuda|BN Brunei|\
BO Bolivia|BQ Caribbean Netherlands|BR Brazil|BS Bahamas|BT Bhutan|BW Botswana|BY Belarus|BZ Belize|\
CA Canada|CD DR Congo|CF Central African Republic|CG Congo|CH Switzerland|CI Ivory Coast|CK Cook Islands|\
CL Chile|CM Cameroon|CN China|CO Colombia|CR Costa Rica|CU Cuba|CV Cape Verde|CW Curacao|CY Cyprus|\
CZ Czechia|DE Germany|DJ Djibouti|DK Denmark|DM Dominica|DO Dominican Republic|DZ Algeria|EC Ecuador|\
EE Estonia|EG Egypt|EH Western Sahara|ER Eritrea|ES Spain|ET Ethiopia|FI Finland|FJ Fiji|\
FK Falkland Islands|FM Micronesia|FO Faroe Islands|FR France|GA Gabon|GB United Kingdom|GD Grenada|\
GE Georgia|GF French Guiana|GG Guernsey|GH Ghana|GI Gibraltar|GL Greenland|GM Gambia|GN Guinea|\
GP Guadeloupe|GQ Equatorial Guinea|GR Greece|GT Guatemala|GU Guam|GW Guinea-Bissau|GY Guyana|\
HK Hong Kong|HN Honduras|HR Croatia|HT Haiti|HU Hungary|ID Indonesia|IE Ireland|IL Israel|\
IM Isle of Man|IN India|IQ Iraq|IR Iran|IS Iceland|IT Italy|JE Jersey|JM Jamaica|JO Jordan|JP Japan|\
KE Kenya|KG Kyrgyzstan|KH Cambodia|KI Kiribati|KM Comoros|KN Saint Kitts and Nevis|KP North Korea|\
KR South Korea|KW Kuwait|KY Cayman Islands|KZ Kazakhstan|LA Laos|LB Lebanon|LC Saint Lucia|\
LI Liechtenstein|LK Sri Lanka|LR Liberia|LS Lesotho|LT Lithuania|LU Luxembourg|LV Latvia|LY Libya|\
MA Morocco|MC Monaco|MD Moldova|ME Montenegro|MF Saint Martin|MG Madagascar|MH Marshall Islands|\
MK North Macedonia|ML Mali|MM Myanmar|MN Mongolia|MO Macao|MP Northern Mariana Islands|MQ Martinique|\
MR Mauritania|MS Montserrat|MT Malta|MU Mauritius|MV Maldives|MW Malawi|MX Mexico|MY Malaysia|\
MZ Mozambique|NA Namibia|NC New Caledonia|NE Niger|NF Norfolk Island|NG Nigeria|NI Nicaragua|\
NL Netherlands|NO Norway|NP Nepal|NR Nauru|NU Niue|NZ New Zealand|OM Oman|PA Panama|PE Peru|\
PF French Polynesia|PG Papua New Guinea|PH Philippines|PK Pakistan|PL Poland|PM Saint Pierre and Miquelon|\
PR Puerto Rico|PS Palestine|PT Portugal|PW Palau|PY Paraguay|QA Qatar|RE Reunion|RO Romania|RS Serbia|\
RU Russia|RW Rwanda|SA Saudi Arabia|SB Solomon Islands|SC Seychelles|SD Sudan|SE Sweden|SG Singapore|\
SH Saint Helena|SI Slovenia|SJ Svalbard and Jan Mayen|SK Slovakia|SL Sierra Leone|SM San Marino|\
SN Senegal|SO Somalia|SR Suriname|SS South Sudan|ST Sao Tome and Principe|SV El Salvador|SX Sint Maarten|\
SY Syria|SZ Eswatini|TC Turks and Caicos Islands|TD Chad|TG Togo|TH Thailand|TJ Tajikistan|TL Timor-Leste|\
TM Turkmenistan|TN Tunisia|TO Tonga|TR Turkey|TT Trinidad and Tobago|TV Tuvalu|TW Taiwan|TZ Tanzania|\
UA Ukraine|UG Uganda|US United States|UY Uruguay|UZ Uzbekistan|VA Vatican City|\
VC Saint Vincent and the Grenadines|VE Venezuela|VG British Virgin Islands|VI US Virgin Islands|\
VN Vietnam|VU Vanuatu|WF Wallis and Futuna|WS Samoa|XK Kosovo|YE Yemen|YT Mayotte|ZA South Africa|\
ZM Zambia|ZW Zimbabwe";

pub fn country_name(code: &str) -> Option<&'static str> {
    COUNTRIES
        .split('|')
        .find_map(|entry| entry.strip_prefix(code)?.strip_prefix(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_positions_become_place_names() {
        let mut g = Geocoder::default();
        // Sedona, Arizona.
        let place = g.place(34.8697, -111.7610).unwrap();
        assert!(
            place.display.ends_with("Arizona, United States"),
            "{}",
            place.display
        );
        assert!(place.searchable.contains(" AZ"));
        // Paris.
        let place = g.place(48.8566, 2.3522).unwrap();
        assert!(place.display.ends_with("France"), "{}", place.display);
        // No position (0, 0) and nonsense are ignored.
        assert_eq!(g.place(0.0, 0.0), None);
        assert_eq!(g.place(91.0, 0.0), None);
        g.release();
    }

    #[test]
    fn state_names_and_abbreviations_find_each_other() {
        assert_eq!(synonyms("arizona"), ["az"]);
        assert_eq!(synonyms("AZ"), ["arizona"]);
        // Lowercase two-letter words are ordinary words.
        assert!(synonyms("in").is_empty());
        assert!(synonyms("or").is_empty());
        assert_eq!(country_name("US"), Some("United States"));
        assert_eq!(country_name("GB"), Some("United Kingdom"));
        assert_eq!(country_name("Q"), None);
    }
}
