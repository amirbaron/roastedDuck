//! The time zone ids of ICU's `zoneinfo64` (tzdata 2026d) in the order
//! `TimeZone::createEnumeration` lists them, generated from DuckDB 1.5.6's ICU data.

/// One ICU time zone id.
#[derive(Clone, Copy, Debug)]
pub struct ZoneName {
    /// The id (case-sensitive).
    pub name: &'static str,
    /// The shortest, lexicographically first equivalent id without a slash (the
    /// `abbrev` of `pg_timezone_names()`).
    pub abbrev: &'static str,
    /// The tzdb zone that has its data: a name in jiff's bundled tzdb, or `posix:` and
    /// a POSIX TZ string for the `SystemV` zones the tzdb no longer has.
    pub data: &'static str,
}

/// `TimeZone::createEnumeration()`.
pub static ZONE_NAMES: &[ZoneName] = &[
    ZoneName {
        name: "ACT",
        abbrev: "ACT",
        data: "Australia/Darwin",
    },
    ZoneName {
        name: "AET",
        abbrev: "AET",
        data: "Australia/Sydney",
    },
    ZoneName {
        name: "AGT",
        abbrev: "AGT",
        data: "America/Argentina/Buenos_Aires",
    },
    ZoneName {
        name: "ART",
        abbrev: "ART",
        data: "Africa/Cairo",
    },
    ZoneName {
        name: "AST",
        abbrev: "AST",
        data: "America/Anchorage",
    },
    ZoneName {
        name: "Africa/Abidjan",
        abbrev: "Iceland",
        data: "Africa/Abidjan",
    },
    ZoneName {
        name: "Africa/Accra",
        abbrev: "Iceland",
        data: "Africa/Accra",
    },
    ZoneName {
        name: "Africa/Addis_Ababa",
        abbrev: "EAT",
        data: "Africa/Addis_Ababa",
    },
    ZoneName {
        name: "Africa/Algiers",
        abbrev: "Africa/Algiers",
        data: "Africa/Algiers",
    },
    ZoneName {
        name: "Africa/Asmara",
        abbrev: "EAT",
        data: "Africa/Asmara",
    },
    ZoneName {
        name: "Africa/Asmera",
        abbrev: "EAT",
        data: "Africa/Asmera",
    },
    ZoneName {
        name: "Africa/Bamako",
        abbrev: "Iceland",
        data: "Africa/Bamako",
    },
    ZoneName {
        name: "Africa/Bangui",
        abbrev: "Africa/Bangui",
        data: "Africa/Bangui",
    },
    ZoneName {
        name: "Africa/Banjul",
        abbrev: "Iceland",
        data: "Africa/Banjul",
    },
    ZoneName {
        name: "Africa/Bissau",
        abbrev: "Africa/Bissau",
        data: "Africa/Bissau",
    },
    ZoneName {
        name: "Africa/Blantyre",
        abbrev: "CAT",
        data: "Africa/Blantyre",
    },
    ZoneName {
        name: "Africa/Brazzaville",
        abbrev: "Africa/Brazzaville",
        data: "Africa/Brazzaville",
    },
    ZoneName {
        name: "Africa/Bujumbura",
        abbrev: "CAT",
        data: "Africa/Bujumbura",
    },
    ZoneName {
        name: "Africa/Cairo",
        abbrev: "ART",
        data: "Africa/Cairo",
    },
    ZoneName {
        name: "Africa/Casablanca",
        abbrev: "Africa/Casablanca",
        data: "Africa/Casablanca",
    },
    ZoneName {
        name: "Africa/Ceuta",
        abbrev: "Africa/Ceuta",
        data: "Africa/Ceuta",
    },
    ZoneName {
        name: "Africa/Conakry",
        abbrev: "Iceland",
        data: "Africa/Conakry",
    },
    ZoneName {
        name: "Africa/Dakar",
        abbrev: "Iceland",
        data: "Africa/Dakar",
    },
    ZoneName {
        name: "Africa/Dar_es_Salaam",
        abbrev: "EAT",
        data: "Africa/Dar_es_Salaam",
    },
    ZoneName {
        name: "Africa/Djibouti",
        abbrev: "EAT",
        data: "Africa/Djibouti",
    },
    ZoneName {
        name: "Africa/Douala",
        abbrev: "Africa/Douala",
        data: "Africa/Douala",
    },
    ZoneName {
        name: "Africa/El_Aaiun",
        abbrev: "Africa/El_Aaiun",
        data: "Africa/El_Aaiun",
    },
    ZoneName {
        name: "Africa/Freetown",
        abbrev: "Iceland",
        data: "Africa/Freetown",
    },
    ZoneName {
        name: "Africa/Gaborone",
        abbrev: "CAT",
        data: "Africa/Gaborone",
    },
    ZoneName {
        name: "Africa/Harare",
        abbrev: "CAT",
        data: "Africa/Harare",
    },
    ZoneName {
        name: "Africa/Johannesburg",
        abbrev: "Africa/Johannesburg",
        data: "Africa/Johannesburg",
    },
    ZoneName {
        name: "Africa/Juba",
        abbrev: "Africa/Juba",
        data: "Africa/Juba",
    },
    ZoneName {
        name: "Africa/Kampala",
        abbrev: "EAT",
        data: "Africa/Kampala",
    },
    ZoneName {
        name: "Africa/Khartoum",
        abbrev: "Africa/Khartoum",
        data: "Africa/Khartoum",
    },
    ZoneName {
        name: "Africa/Kigali",
        abbrev: "CAT",
        data: "Africa/Kigali",
    },
    ZoneName {
        name: "Africa/Kinshasa",
        abbrev: "Africa/Kinshasa",
        data: "Africa/Kinshasa",
    },
    ZoneName {
        name: "Africa/Lagos",
        abbrev: "Africa/Lagos",
        data: "Africa/Lagos",
    },
    ZoneName {
        name: "Africa/Libreville",
        abbrev: "Africa/Libreville",
        data: "Africa/Libreville",
    },
    ZoneName {
        name: "Africa/Lome",
        abbrev: "Iceland",
        data: "Africa/Lome",
    },
    ZoneName {
        name: "Africa/Luanda",
        abbrev: "Africa/Luanda",
        data: "Africa/Luanda",
    },
    ZoneName {
        name: "Africa/Lubumbashi",
        abbrev: "CAT",
        data: "Africa/Lubumbashi",
    },
    ZoneName {
        name: "Africa/Lusaka",
        abbrev: "CAT",
        data: "Africa/Lusaka",
    },
    ZoneName {
        name: "Africa/Malabo",
        abbrev: "Africa/Malabo",
        data: "Africa/Malabo",
    },
    ZoneName {
        name: "Africa/Maputo",
        abbrev: "CAT",
        data: "Africa/Maputo",
    },
    ZoneName {
        name: "Africa/Maseru",
        abbrev: "Africa/Maseru",
        data: "Africa/Maseru",
    },
    ZoneName {
        name: "Africa/Mbabane",
        abbrev: "Africa/Mbabane",
        data: "Africa/Mbabane",
    },
    ZoneName {
        name: "Africa/Mogadishu",
        abbrev: "EAT",
        data: "Africa/Mogadishu",
    },
    ZoneName {
        name: "Africa/Monrovia",
        abbrev: "Africa/Monrovia",
        data: "Africa/Monrovia",
    },
    ZoneName {
        name: "Africa/Nairobi",
        abbrev: "EAT",
        data: "Africa/Nairobi",
    },
    ZoneName {
        name: "Africa/Ndjamena",
        abbrev: "Africa/Ndjamena",
        data: "Africa/Ndjamena",
    },
    ZoneName {
        name: "Africa/Niamey",
        abbrev: "Africa/Niamey",
        data: "Africa/Niamey",
    },
    ZoneName {
        name: "Africa/Nouakchott",
        abbrev: "Iceland",
        data: "Africa/Nouakchott",
    },
    ZoneName {
        name: "Africa/Ouagadougou",
        abbrev: "Iceland",
        data: "Africa/Ouagadougou",
    },
    ZoneName {
        name: "Africa/Porto-Novo",
        abbrev: "Africa/Porto-Novo",
        data: "Africa/Porto-Novo",
    },
    ZoneName {
        name: "Africa/Sao_Tome",
        abbrev: "Africa/Sao_Tome",
        data: "Africa/Sao_Tome",
    },
    ZoneName {
        name: "Africa/Timbuktu",
        abbrev: "Iceland",
        data: "Africa/Timbuktu",
    },
    ZoneName {
        name: "Africa/Tripoli",
        abbrev: "Libya",
        data: "Africa/Tripoli",
    },
    ZoneName {
        name: "Africa/Tunis",
        abbrev: "Africa/Tunis",
        data: "Africa/Tunis",
    },
    ZoneName {
        name: "Africa/Windhoek",
        abbrev: "Africa/Windhoek",
        data: "Africa/Windhoek",
    },
    ZoneName {
        name: "America/Adak",
        abbrev: "America/Adak",
        data: "America/Adak",
    },
    ZoneName {
        name: "America/Anchorage",
        abbrev: "AST",
        data: "America/Anchorage",
    },
    ZoneName {
        name: "America/Anguilla",
        abbrev: "PRT",
        data: "America/Anguilla",
    },
    ZoneName {
        name: "America/Antigua",
        abbrev: "PRT",
        data: "America/Antigua",
    },
    ZoneName {
        name: "America/Araguaina",
        abbrev: "America/Araguaina",
        data: "America/Araguaina",
    },
    ZoneName {
        name: "America/Argentina/Buenos_Aires",
        abbrev: "AGT",
        data: "America/Argentina/Buenos_Aires",
    },
    ZoneName {
        name: "America/Argentina/Catamarca",
        abbrev: "America/Argentina/Catamarca",
        data: "America/Argentina/Catamarca",
    },
    ZoneName {
        name: "America/Argentina/ComodRivadavia",
        abbrev: "America/Argentina/ComodRivadavia",
        data: "America/Argentina/ComodRivadavia",
    },
    ZoneName {
        name: "America/Argentina/Cordoba",
        abbrev: "America/Argentina/Cordoba",
        data: "America/Argentina/Cordoba",
    },
    ZoneName {
        name: "America/Argentina/Jujuy",
        abbrev: "America/Argentina/Jujuy",
        data: "America/Argentina/Jujuy",
    },
    ZoneName {
        name: "America/Argentina/La_Rioja",
        abbrev: "America/Argentina/La_Rioja",
        data: "America/Argentina/La_Rioja",
    },
    ZoneName {
        name: "America/Argentina/Mendoza",
        abbrev: "America/Argentina/Mendoza",
        data: "America/Argentina/Mendoza",
    },
    ZoneName {
        name: "America/Argentina/Rio_Gallegos",
        abbrev: "America/Argentina/Rio_Gallegos",
        data: "America/Argentina/Rio_Gallegos",
    },
    ZoneName {
        name: "America/Argentina/Salta",
        abbrev: "America/Argentina/Salta",
        data: "America/Argentina/Salta",
    },
    ZoneName {
        name: "America/Argentina/San_Juan",
        abbrev: "America/Argentina/San_Juan",
        data: "America/Argentina/San_Juan",
    },
    ZoneName {
        name: "America/Argentina/San_Luis",
        abbrev: "America/Argentina/San_Luis",
        data: "America/Argentina/San_Luis",
    },
    ZoneName {
        name: "America/Argentina/Tucuman",
        abbrev: "America/Argentina/Tucuman",
        data: "America/Argentina/Tucuman",
    },
    ZoneName {
        name: "America/Argentina/Ushuaia",
        abbrev: "America/Argentina/Ushuaia",
        data: "America/Argentina/Ushuaia",
    },
    ZoneName {
        name: "America/Aruba",
        abbrev: "PRT",
        data: "America/Aruba",
    },
    ZoneName {
        name: "America/Asuncion",
        abbrev: "America/Asuncion",
        data: "America/Asuncion",
    },
    ZoneName {
        name: "America/Atikokan",
        abbrev: "EST",
        data: "America/Atikokan",
    },
    ZoneName {
        name: "America/Atka",
        abbrev: "America/Atka",
        data: "America/Atka",
    },
    ZoneName {
        name: "America/Bahia",
        abbrev: "America/Bahia",
        data: "America/Bahia",
    },
    ZoneName {
        name: "America/Bahia_Banderas",
        abbrev: "America/Bahia_Banderas",
        data: "America/Bahia_Banderas",
    },
    ZoneName {
        name: "America/Barbados",
        abbrev: "America/Barbados",
        data: "America/Barbados",
    },
    ZoneName {
        name: "America/Belem",
        abbrev: "America/Belem",
        data: "America/Belem",
    },
    ZoneName {
        name: "America/Belize",
        abbrev: "America/Belize",
        data: "America/Belize",
    },
    ZoneName {
        name: "America/Blanc-Sablon",
        abbrev: "PRT",
        data: "America/Blanc-Sablon",
    },
    ZoneName {
        name: "America/Boa_Vista",
        abbrev: "America/Boa_Vista",
        data: "America/Boa_Vista",
    },
    ZoneName {
        name: "America/Bogota",
        abbrev: "America/Bogota",
        data: "America/Bogota",
    },
    ZoneName {
        name: "America/Boise",
        abbrev: "America/Boise",
        data: "America/Boise",
    },
    ZoneName {
        name: "America/Buenos_Aires",
        abbrev: "AGT",
        data: "America/Buenos_Aires",
    },
    ZoneName {
        name: "America/Cambridge_Bay",
        abbrev: "America/Cambridge_Bay",
        data: "America/Cambridge_Bay",
    },
    ZoneName {
        name: "America/Campo_Grande",
        abbrev: "America/Campo_Grande",
        data: "America/Campo_Grande",
    },
    ZoneName {
        name: "America/Cancun",
        abbrev: "America/Cancun",
        data: "America/Cancun",
    },
    ZoneName {
        name: "America/Caracas",
        abbrev: "America/Caracas",
        data: "America/Caracas",
    },
    ZoneName {
        name: "America/Catamarca",
        abbrev: "America/Catamarca",
        data: "America/Catamarca",
    },
    ZoneName {
        name: "America/Cayenne",
        abbrev: "America/Cayenne",
        data: "America/Cayenne",
    },
    ZoneName {
        name: "America/Cayman",
        abbrev: "EST",
        data: "America/Cayman",
    },
    ZoneName {
        name: "America/Chicago",
        abbrev: "CST",
        data: "America/Chicago",
    },
    ZoneName {
        name: "America/Chihuahua",
        abbrev: "America/Chihuahua",
        data: "America/Chihuahua",
    },
    ZoneName {
        name: "America/Ciudad_Juarez",
        abbrev: "America/Ciudad_Juarez",
        data: "America/Ciudad_Juarez",
    },
    ZoneName {
        name: "America/Coral_Harbour",
        abbrev: "EST",
        data: "America/Coral_Harbour",
    },
    ZoneName {
        name: "America/Cordoba",
        abbrev: "America/Cordoba",
        data: "America/Cordoba",
    },
    ZoneName {
        name: "America/Costa_Rica",
        abbrev: "America/Costa_Rica",
        data: "America/Costa_Rica",
    },
    ZoneName {
        name: "America/Coyhaique",
        abbrev: "America/Coyhaique",
        data: "America/Coyhaique",
    },
    ZoneName {
        name: "America/Creston",
        abbrev: "MST",
        data: "America/Creston",
    },
    ZoneName {
        name: "America/Cuiaba",
        abbrev: "America/Cuiaba",
        data: "America/Cuiaba",
    },
    ZoneName {
        name: "America/Curacao",
        abbrev: "PRT",
        data: "America/Curacao",
    },
    ZoneName {
        name: "America/Danmarkshavn",
        abbrev: "America/Danmarkshavn",
        data: "America/Danmarkshavn",
    },
    ZoneName {
        name: "America/Dawson",
        abbrev: "America/Dawson",
        data: "America/Dawson",
    },
    ZoneName {
        name: "America/Dawson_Creek",
        abbrev: "America/Dawson_Creek",
        data: "America/Dawson_Creek",
    },
    ZoneName {
        name: "America/Denver",
        abbrev: "Navajo",
        data: "America/Denver",
    },
    ZoneName {
        name: "America/Detroit",
        abbrev: "America/Detroit",
        data: "America/Detroit",
    },
    ZoneName {
        name: "America/Dominica",
        abbrev: "PRT",
        data: "America/Dominica",
    },
    ZoneName {
        name: "America/Edmonton",
        abbrev: "America/Edmonton",
        data: "America/Edmonton",
    },
    ZoneName {
        name: "America/Eirunepe",
        abbrev: "America/Eirunepe",
        data: "America/Eirunepe",
    },
    ZoneName {
        name: "America/El_Salvador",
        abbrev: "America/El_Salvador",
        data: "America/El_Salvador",
    },
    ZoneName {
        name: "America/Ensenada",
        abbrev: "America/Ensenada",
        data: "America/Ensenada",
    },
    ZoneName {
        name: "America/Fort_Nelson",
        abbrev: "America/Fort_Nelson",
        data: "America/Fort_Nelson",
    },
    ZoneName {
        name: "America/Fort_Wayne",
        abbrev: "IET",
        data: "America/Fort_Wayne",
    },
    ZoneName {
        name: "America/Fortaleza",
        abbrev: "America/Fortaleza",
        data: "America/Fortaleza",
    },
    ZoneName {
        name: "America/Glace_Bay",
        abbrev: "America/Glace_Bay",
        data: "America/Glace_Bay",
    },
    ZoneName {
        name: "America/Godthab",
        abbrev: "America/Godthab",
        data: "America/Godthab",
    },
    ZoneName {
        name: "America/Goose_Bay",
        abbrev: "America/Goose_Bay",
        data: "America/Goose_Bay",
    },
    ZoneName {
        name: "America/Grand_Turk",
        abbrev: "America/Grand_Turk",
        data: "America/Grand_Turk",
    },
    ZoneName {
        name: "America/Grenada",
        abbrev: "PRT",
        data: "America/Grenada",
    },
    ZoneName {
        name: "America/Guadeloupe",
        abbrev: "PRT",
        data: "America/Guadeloupe",
    },
    ZoneName {
        name: "America/Guatemala",
        abbrev: "America/Guatemala",
        data: "America/Guatemala",
    },
    ZoneName {
        name: "America/Guayaquil",
        abbrev: "America/Guayaquil",
        data: "America/Guayaquil",
    },
    ZoneName {
        name: "America/Guyana",
        abbrev: "America/Guyana",
        data: "America/Guyana",
    },
    ZoneName {
        name: "America/Halifax",
        abbrev: "America/Halifax",
        data: "America/Halifax",
    },
    ZoneName {
        name: "America/Havana",
        abbrev: "Cuba",
        data: "America/Havana",
    },
    ZoneName {
        name: "America/Hermosillo",
        abbrev: "America/Hermosillo",
        data: "America/Hermosillo",
    },
    ZoneName {
        name: "America/Indiana/Indianapolis",
        abbrev: "IET",
        data: "America/Indiana/Indianapolis",
    },
    ZoneName {
        name: "America/Indiana/Knox",
        abbrev: "America/Indiana/Knox",
        data: "America/Indiana/Knox",
    },
    ZoneName {
        name: "America/Indiana/Marengo",
        abbrev: "America/Indiana/Marengo",
        data: "America/Indiana/Marengo",
    },
    ZoneName {
        name: "America/Indiana/Petersburg",
        abbrev: "America/Indiana/Petersburg",
        data: "America/Indiana/Petersburg",
    },
    ZoneName {
        name: "America/Indiana/Tell_City",
        abbrev: "America/Indiana/Tell_City",
        data: "America/Indiana/Tell_City",
    },
    ZoneName {
        name: "America/Indiana/Vevay",
        abbrev: "America/Indiana/Vevay",
        data: "America/Indiana/Vevay",
    },
    ZoneName {
        name: "America/Indiana/Vincennes",
        abbrev: "America/Indiana/Vincennes",
        data: "America/Indiana/Vincennes",
    },
    ZoneName {
        name: "America/Indiana/Winamac",
        abbrev: "America/Indiana/Winamac",
        data: "America/Indiana/Winamac",
    },
    ZoneName {
        name: "America/Indianapolis",
        abbrev: "IET",
        data: "America/Indianapolis",
    },
    ZoneName {
        name: "America/Inuvik",
        abbrev: "America/Inuvik",
        data: "America/Inuvik",
    },
    ZoneName {
        name: "America/Iqaluit",
        abbrev: "America/Iqaluit",
        data: "America/Iqaluit",
    },
    ZoneName {
        name: "America/Jamaica",
        abbrev: "Jamaica",
        data: "America/Jamaica",
    },
    ZoneName {
        name: "America/Jujuy",
        abbrev: "America/Jujuy",
        data: "America/Jujuy",
    },
    ZoneName {
        name: "America/Juneau",
        abbrev: "America/Juneau",
        data: "America/Juneau",
    },
    ZoneName {
        name: "America/Kentucky/Louisville",
        abbrev: "America/Kentucky/Louisville",
        data: "America/Kentucky/Louisville",
    },
    ZoneName {
        name: "America/Kentucky/Monticello",
        abbrev: "America/Kentucky/Monticello",
        data: "America/Kentucky/Monticello",
    },
    ZoneName {
        name: "America/Knox_IN",
        abbrev: "America/Knox_IN",
        data: "America/Knox_IN",
    },
    ZoneName {
        name: "America/Kralendijk",
        abbrev: "PRT",
        data: "America/Kralendijk",
    },
    ZoneName {
        name: "America/La_Paz",
        abbrev: "America/La_Paz",
        data: "America/La_Paz",
    },
    ZoneName {
        name: "America/Lima",
        abbrev: "America/Lima",
        data: "America/Lima",
    },
    ZoneName {
        name: "America/Los_Angeles",
        abbrev: "PST",
        data: "America/Los_Angeles",
    },
    ZoneName {
        name: "America/Louisville",
        abbrev: "America/Louisville",
        data: "America/Louisville",
    },
    ZoneName {
        name: "America/Lower_Princes",
        abbrev: "PRT",
        data: "America/Lower_Princes",
    },
    ZoneName {
        name: "America/Maceio",
        abbrev: "America/Maceio",
        data: "America/Maceio",
    },
    ZoneName {
        name: "America/Managua",
        abbrev: "America/Managua",
        data: "America/Managua",
    },
    ZoneName {
        name: "America/Manaus",
        abbrev: "America/Manaus",
        data: "America/Manaus",
    },
    ZoneName {
        name: "America/Marigot",
        abbrev: "PRT",
        data: "America/Marigot",
    },
    ZoneName {
        name: "America/Martinique",
        abbrev: "America/Martinique",
        data: "America/Martinique",
    },
    ZoneName {
        name: "America/Matamoros",
        abbrev: "America/Matamoros",
        data: "America/Matamoros",
    },
    ZoneName {
        name: "America/Mazatlan",
        abbrev: "America/Mazatlan",
        data: "America/Mazatlan",
    },
    ZoneName {
        name: "America/Mendoza",
        abbrev: "America/Mendoza",
        data: "America/Mendoza",
    },
    ZoneName {
        name: "America/Menominee",
        abbrev: "America/Menominee",
        data: "America/Menominee",
    },
    ZoneName {
        name: "America/Merida",
        abbrev: "America/Merida",
        data: "America/Merida",
    },
    ZoneName {
        name: "America/Metlakatla",
        abbrev: "America/Metlakatla",
        data: "America/Metlakatla",
    },
    ZoneName {
        name: "America/Mexico_City",
        abbrev: "America/Mexico_City",
        data: "America/Mexico_City",
    },
    ZoneName {
        name: "America/Miquelon",
        abbrev: "America/Miquelon",
        data: "America/Miquelon",
    },
    ZoneName {
        name: "America/Moncton",
        abbrev: "America/Moncton",
        data: "America/Moncton",
    },
    ZoneName {
        name: "America/Monterrey",
        abbrev: "America/Monterrey",
        data: "America/Monterrey",
    },
    ZoneName {
        name: "America/Montevideo",
        abbrev: "America/Montevideo",
        data: "America/Montevideo",
    },
    ZoneName {
        name: "America/Montreal",
        abbrev: "America/Montreal",
        data: "America/Montreal",
    },
    ZoneName {
        name: "America/Montserrat",
        abbrev: "PRT",
        data: "America/Montserrat",
    },
    ZoneName {
        name: "America/Nassau",
        abbrev: "America/Nassau",
        data: "America/Nassau",
    },
    ZoneName {
        name: "America/New_York",
        abbrev: "America/New_York",
        data: "America/New_York",
    },
    ZoneName {
        name: "America/Nipigon",
        abbrev: "America/Nipigon",
        data: "America/Nipigon",
    },
    ZoneName {
        name: "America/Nome",
        abbrev: "America/Nome",
        data: "America/Nome",
    },
    ZoneName {
        name: "America/Noronha",
        abbrev: "America/Noronha",
        data: "America/Noronha",
    },
    ZoneName {
        name: "America/North_Dakota/Beulah",
        abbrev: "America/North_Dakota/Beulah",
        data: "America/North_Dakota/Beulah",
    },
    ZoneName {
        name: "America/North_Dakota/Center",
        abbrev: "America/North_Dakota/Center",
        data: "America/North_Dakota/Center",
    },
    ZoneName {
        name: "America/North_Dakota/New_Salem",
        abbrev: "America/North_Dakota/New_Salem",
        data: "America/North_Dakota/New_Salem",
    },
    ZoneName {
        name: "America/Nuuk",
        abbrev: "America/Nuuk",
        data: "America/Nuuk",
    },
    ZoneName {
        name: "America/Ojinaga",
        abbrev: "America/Ojinaga",
        data: "America/Ojinaga",
    },
    ZoneName {
        name: "America/Panama",
        abbrev: "EST",
        data: "America/Panama",
    },
    ZoneName {
        name: "America/Pangnirtung",
        abbrev: "America/Pangnirtung",
        data: "America/Pangnirtung",
    },
    ZoneName {
        name: "America/Paramaribo",
        abbrev: "America/Paramaribo",
        data: "America/Paramaribo",
    },
    ZoneName {
        name: "America/Phoenix",
        abbrev: "MST",
        data: "America/Phoenix",
    },
    ZoneName {
        name: "America/Port-au-Prince",
        abbrev: "America/Port-au-Prince",
        data: "America/Port-au-Prince",
    },
    ZoneName {
        name: "America/Port_of_Spain",
        abbrev: "PRT",
        data: "America/Port_of_Spain",
    },
    ZoneName {
        name: "America/Porto_Acre",
        abbrev: "America/Porto_Acre",
        data: "America/Porto_Acre",
    },
    ZoneName {
        name: "America/Porto_Velho",
        abbrev: "America/Porto_Velho",
        data: "America/Porto_Velho",
    },
    ZoneName {
        name: "America/Puerto_Rico",
        abbrev: "PRT",
        data: "America/Puerto_Rico",
    },
    ZoneName {
        name: "America/Punta_Arenas",
        abbrev: "America/Punta_Arenas",
        data: "America/Punta_Arenas",
    },
    ZoneName {
        name: "America/Rainy_River",
        abbrev: "America/Rainy_River",
        data: "America/Rainy_River",
    },
    ZoneName {
        name: "America/Rankin_Inlet",
        abbrev: "America/Rankin_Inlet",
        data: "America/Rankin_Inlet",
    },
    ZoneName {
        name: "America/Recife",
        abbrev: "America/Recife",
        data: "America/Recife",
    },
    ZoneName {
        name: "America/Regina",
        abbrev: "America/Regina",
        data: "America/Regina",
    },
    ZoneName {
        name: "America/Resolute",
        abbrev: "America/Resolute",
        data: "America/Resolute",
    },
    ZoneName {
        name: "America/Rio_Branco",
        abbrev: "America/Rio_Branco",
        data: "America/Rio_Branco",
    },
    ZoneName {
        name: "America/Rosario",
        abbrev: "America/Rosario",
        data: "America/Rosario",
    },
    ZoneName {
        name: "America/Santa_Isabel",
        abbrev: "America/Santa_Isabel",
        data: "America/Santa_Isabel",
    },
    ZoneName {
        name: "America/Santarem",
        abbrev: "America/Santarem",
        data: "America/Santarem",
    },
    ZoneName {
        name: "America/Santiago",
        abbrev: "America/Santiago",
        data: "America/Santiago",
    },
    ZoneName {
        name: "America/Santo_Domingo",
        abbrev: "America/Santo_Domingo",
        data: "America/Santo_Domingo",
    },
    ZoneName {
        name: "America/Sao_Paulo",
        abbrev: "BET",
        data: "America/Sao_Paulo",
    },
    ZoneName {
        name: "America/Scoresbysund",
        abbrev: "America/Scoresbysund",
        data: "America/Scoresbysund",
    },
    ZoneName {
        name: "America/Shiprock",
        abbrev: "Navajo",
        data: "America/Shiprock",
    },
    ZoneName {
        name: "America/Sitka",
        abbrev: "America/Sitka",
        data: "America/Sitka",
    },
    ZoneName {
        name: "America/St_Barthelemy",
        abbrev: "PRT",
        data: "America/St_Barthelemy",
    },
    ZoneName {
        name: "America/St_Johns",
        abbrev: "CNT",
        data: "America/St_Johns",
    },
    ZoneName {
        name: "America/St_Kitts",
        abbrev: "PRT",
        data: "America/St_Kitts",
    },
    ZoneName {
        name: "America/St_Lucia",
        abbrev: "PRT",
        data: "America/St_Lucia",
    },
    ZoneName {
        name: "America/St_Thomas",
        abbrev: "PRT",
        data: "America/St_Thomas",
    },
    ZoneName {
        name: "America/St_Vincent",
        abbrev: "PRT",
        data: "America/St_Vincent",
    },
    ZoneName {
        name: "America/Swift_Current",
        abbrev: "America/Swift_Current",
        data: "America/Swift_Current",
    },
    ZoneName {
        name: "America/Tegucigalpa",
        abbrev: "America/Tegucigalpa",
        data: "America/Tegucigalpa",
    },
    ZoneName {
        name: "America/Thule",
        abbrev: "America/Thule",
        data: "America/Thule",
    },
    ZoneName {
        name: "America/Thunder_Bay",
        abbrev: "America/Thunder_Bay",
        data: "America/Thunder_Bay",
    },
    ZoneName {
        name: "America/Tijuana",
        abbrev: "America/Tijuana",
        data: "America/Tijuana",
    },
    ZoneName {
        name: "America/Toronto",
        abbrev: "America/Toronto",
        data: "America/Toronto",
    },
    ZoneName {
        name: "America/Tortola",
        abbrev: "PRT",
        data: "America/Tortola",
    },
    ZoneName {
        name: "America/Vancouver",
        abbrev: "America/Vancouver",
        data: "America/Vancouver",
    },
    ZoneName {
        name: "America/Virgin",
        abbrev: "PRT",
        data: "America/Virgin",
    },
    ZoneName {
        name: "America/Whitehorse",
        abbrev: "America/Whitehorse",
        data: "America/Whitehorse",
    },
    ZoneName {
        name: "America/Winnipeg",
        abbrev: "America/Winnipeg",
        data: "America/Winnipeg",
    },
    ZoneName {
        name: "America/Yakutat",
        abbrev: "America/Yakutat",
        data: "America/Yakutat",
    },
    ZoneName {
        name: "America/Yellowknife",
        abbrev: "America/Yellowknife",
        data: "America/Yellowknife",
    },
    ZoneName {
        name: "Antarctica/Casey",
        abbrev: "Antarctica/Casey",
        data: "Antarctica/Casey",
    },
    ZoneName {
        name: "Antarctica/Davis",
        abbrev: "Antarctica/Davis",
        data: "Antarctica/Davis",
    },
    ZoneName {
        name: "Antarctica/DumontDUrville",
        abbrev: "Antarctica/DumontDUrville",
        data: "Antarctica/DumontDUrville",
    },
    ZoneName {
        name: "Antarctica/Macquarie",
        abbrev: "Antarctica/Macquarie",
        data: "Antarctica/Macquarie",
    },
    ZoneName {
        name: "Antarctica/Mawson",
        abbrev: "Antarctica/Mawson",
        data: "Antarctica/Mawson",
    },
    ZoneName {
        name: "Antarctica/McMurdo",
        abbrev: "NZ",
        data: "Antarctica/McMurdo",
    },
    ZoneName {
        name: "Antarctica/Palmer",
        abbrev: "Antarctica/Palmer",
        data: "Antarctica/Palmer",
    },
    ZoneName {
        name: "Antarctica/Rothera",
        abbrev: "Antarctica/Rothera",
        data: "Antarctica/Rothera",
    },
    ZoneName {
        name: "Antarctica/South_Pole",
        abbrev: "NZ",
        data: "Antarctica/South_Pole",
    },
    ZoneName {
        name: "Antarctica/Syowa",
        abbrev: "Antarctica/Syowa",
        data: "Antarctica/Syowa",
    },
    ZoneName {
        name: "Antarctica/Troll",
        abbrev: "Antarctica/Troll",
        data: "Antarctica/Troll",
    },
    ZoneName {
        name: "Antarctica/Vostok",
        abbrev: "Antarctica/Vostok",
        data: "Antarctica/Vostok",
    },
    ZoneName {
        name: "Arctic/Longyearbyen",
        abbrev: "Arctic/Longyearbyen",
        data: "Arctic/Longyearbyen",
    },
    ZoneName {
        name: "Asia/Aden",
        abbrev: "Asia/Aden",
        data: "Asia/Aden",
    },
    ZoneName {
        name: "Asia/Almaty",
        abbrev: "Asia/Almaty",
        data: "Asia/Almaty",
    },
    ZoneName {
        name: "Asia/Amman",
        abbrev: "Asia/Amman",
        data: "Asia/Amman",
    },
    ZoneName {
        name: "Asia/Anadyr",
        abbrev: "Asia/Anadyr",
        data: "Asia/Anadyr",
    },
    ZoneName {
        name: "Asia/Aqtau",
        abbrev: "Asia/Aqtau",
        data: "Asia/Aqtau",
    },
    ZoneName {
        name: "Asia/Aqtobe",
        abbrev: "Asia/Aqtobe",
        data: "Asia/Aqtobe",
    },
    ZoneName {
        name: "Asia/Ashgabat",
        abbrev: "Asia/Ashgabat",
        data: "Asia/Ashgabat",
    },
    ZoneName {
        name: "Asia/Ashkhabad",
        abbrev: "Asia/Ashkhabad",
        data: "Asia/Ashkhabad",
    },
    ZoneName {
        name: "Asia/Atyrau",
        abbrev: "Asia/Atyrau",
        data: "Asia/Atyrau",
    },
    ZoneName {
        name: "Asia/Baghdad",
        abbrev: "Asia/Baghdad",
        data: "Asia/Baghdad",
    },
    ZoneName {
        name: "Asia/Bahrain",
        abbrev: "Asia/Bahrain",
        data: "Asia/Bahrain",
    },
    ZoneName {
        name: "Asia/Baku",
        abbrev: "Asia/Baku",
        data: "Asia/Baku",
    },
    ZoneName {
        name: "Asia/Bangkok",
        abbrev: "Asia/Bangkok",
        data: "Asia/Bangkok",
    },
    ZoneName {
        name: "Asia/Barnaul",
        abbrev: "Asia/Barnaul",
        data: "Asia/Barnaul",
    },
    ZoneName {
        name: "Asia/Beirut",
        abbrev: "Asia/Beirut",
        data: "Asia/Beirut",
    },
    ZoneName {
        name: "Asia/Bishkek",
        abbrev: "Asia/Bishkek",
        data: "Asia/Bishkek",
    },
    ZoneName {
        name: "Asia/Brunei",
        abbrev: "Asia/Brunei",
        data: "Asia/Brunei",
    },
    ZoneName {
        name: "Asia/Calcutta",
        abbrev: "IST",
        data: "Asia/Calcutta",
    },
    ZoneName {
        name: "Asia/Chita",
        abbrev: "Asia/Chita",
        data: "Asia/Chita",
    },
    ZoneName {
        name: "Asia/Choibalsan",
        abbrev: "Asia/Choibalsan",
        data: "Asia/Choibalsan",
    },
    ZoneName {
        name: "Asia/Chongqing",
        abbrev: "CTT",
        data: "Asia/Chongqing",
    },
    ZoneName {
        name: "Asia/Chungking",
        abbrev: "CTT",
        data: "Asia/Chungking",
    },
    ZoneName {
        name: "Asia/Colombo",
        abbrev: "Asia/Colombo",
        data: "Asia/Colombo",
    },
    ZoneName {
        name: "Asia/Dacca",
        abbrev: "BST",
        data: "Asia/Dacca",
    },
    ZoneName {
        name: "Asia/Damascus",
        abbrev: "Asia/Damascus",
        data: "Asia/Damascus",
    },
    ZoneName {
        name: "Asia/Dhaka",
        abbrev: "BST",
        data: "Asia/Dhaka",
    },
    ZoneName {
        name: "Asia/Dili",
        abbrev: "Asia/Dili",
        data: "Asia/Dili",
    },
    ZoneName {
        name: "Asia/Dubai",
        abbrev: "Asia/Dubai",
        data: "Asia/Dubai",
    },
    ZoneName {
        name: "Asia/Dushanbe",
        abbrev: "Asia/Dushanbe",
        data: "Asia/Dushanbe",
    },
    ZoneName {
        name: "Asia/Famagusta",
        abbrev: "Asia/Famagusta",
        data: "Asia/Famagusta",
    },
    ZoneName {
        name: "Asia/Gaza",
        abbrev: "Asia/Gaza",
        data: "Asia/Gaza",
    },
    ZoneName {
        name: "Asia/Harbin",
        abbrev: "CTT",
        data: "Asia/Harbin",
    },
    ZoneName {
        name: "Asia/Hebron",
        abbrev: "Asia/Hebron",
        data: "Asia/Hebron",
    },
    ZoneName {
        name: "Asia/Ho_Chi_Minh",
        abbrev: "VST",
        data: "Asia/Ho_Chi_Minh",
    },
    ZoneName {
        name: "Asia/Hong_Kong",
        abbrev: "Hongkong",
        data: "Asia/Hong_Kong",
    },
    ZoneName {
        name: "Asia/Hovd",
        abbrev: "Asia/Hovd",
        data: "Asia/Hovd",
    },
    ZoneName {
        name: "Asia/Irkutsk",
        abbrev: "Asia/Irkutsk",
        data: "Asia/Irkutsk",
    },
    ZoneName {
        name: "Asia/Istanbul",
        abbrev: "Turkey",
        data: "Asia/Istanbul",
    },
    ZoneName {
        name: "Asia/Jakarta",
        abbrev: "Asia/Jakarta",
        data: "Asia/Jakarta",
    },
    ZoneName {
        name: "Asia/Jayapura",
        abbrev: "Asia/Jayapura",
        data: "Asia/Jayapura",
    },
    ZoneName {
        name: "Asia/Jerusalem",
        abbrev: "Israel",
        data: "Asia/Jerusalem",
    },
    ZoneName {
        name: "Asia/Kabul",
        abbrev: "Asia/Kabul",
        data: "Asia/Kabul",
    },
    ZoneName {
        name: "Asia/Kamchatka",
        abbrev: "Asia/Kamchatka",
        data: "Asia/Kamchatka",
    },
    ZoneName {
        name: "Asia/Karachi",
        abbrev: "PLT",
        data: "Asia/Karachi",
    },
    ZoneName {
        name: "Asia/Kashgar",
        abbrev: "Asia/Kashgar",
        data: "Asia/Kashgar",
    },
    ZoneName {
        name: "Asia/Kathmandu",
        abbrev: "Asia/Kathmandu",
        data: "Asia/Kathmandu",
    },
    ZoneName {
        name: "Asia/Katmandu",
        abbrev: "Asia/Katmandu",
        data: "Asia/Katmandu",
    },
    ZoneName {
        name: "Asia/Khandyga",
        abbrev: "Asia/Khandyga",
        data: "Asia/Khandyga",
    },
    ZoneName {
        name: "Asia/Kolkata",
        abbrev: "IST",
        data: "Asia/Kolkata",
    },
    ZoneName {
        name: "Asia/Krasnoyarsk",
        abbrev: "Asia/Krasnoyarsk",
        data: "Asia/Krasnoyarsk",
    },
    ZoneName {
        name: "Asia/Kuala_Lumpur",
        abbrev: "Singapore",
        data: "Asia/Kuala_Lumpur",
    },
    ZoneName {
        name: "Asia/Kuching",
        abbrev: "Asia/Kuching",
        data: "Asia/Kuching",
    },
    ZoneName {
        name: "Asia/Kuwait",
        abbrev: "Asia/Kuwait",
        data: "Asia/Kuwait",
    },
    ZoneName {
        name: "Asia/Macao",
        abbrev: "Asia/Macao",
        data: "Asia/Macao",
    },
    ZoneName {
        name: "Asia/Macau",
        abbrev: "Asia/Macau",
        data: "Asia/Macau",
    },
    ZoneName {
        name: "Asia/Magadan",
        abbrev: "Asia/Magadan",
        data: "Asia/Magadan",
    },
    ZoneName {
        name: "Asia/Makassar",
        abbrev: "Asia/Makassar",
        data: "Asia/Makassar",
    },
    ZoneName {
        name: "Asia/Manila",
        abbrev: "Asia/Manila",
        data: "Asia/Manila",
    },
    ZoneName {
        name: "Asia/Muscat",
        abbrev: "Asia/Muscat",
        data: "Asia/Muscat",
    },
    ZoneName {
        name: "Asia/Nicosia",
        abbrev: "Asia/Nicosia",
        data: "Asia/Nicosia",
    },
    ZoneName {
        name: "Asia/Novokuznetsk",
        abbrev: "Asia/Novokuznetsk",
        data: "Asia/Novokuznetsk",
    },
    ZoneName {
        name: "Asia/Novosibirsk",
        abbrev: "Asia/Novosibirsk",
        data: "Asia/Novosibirsk",
    },
    ZoneName {
        name: "Asia/Omsk",
        abbrev: "Asia/Omsk",
        data: "Asia/Omsk",
    },
    ZoneName {
        name: "Asia/Oral",
        abbrev: "Asia/Oral",
        data: "Asia/Oral",
    },
    ZoneName {
        name: "Asia/Phnom_Penh",
        abbrev: "Asia/Phnom_Penh",
        data: "Asia/Phnom_Penh",
    },
    ZoneName {
        name: "Asia/Pontianak",
        abbrev: "Asia/Pontianak",
        data: "Asia/Pontianak",
    },
    ZoneName {
        name: "Asia/Pyongyang",
        abbrev: "Asia/Pyongyang",
        data: "Asia/Pyongyang",
    },
    ZoneName {
        name: "Asia/Qatar",
        abbrev: "Asia/Qatar",
        data: "Asia/Qatar",
    },
    ZoneName {
        name: "Asia/Qostanay",
        abbrev: "Asia/Qostanay",
        data: "Asia/Qostanay",
    },
    ZoneName {
        name: "Asia/Qyzylorda",
        abbrev: "Asia/Qyzylorda",
        data: "Asia/Qyzylorda",
    },
    ZoneName {
        name: "Asia/Rangoon",
        abbrev: "Asia/Rangoon",
        data: "Asia/Rangoon",
    },
    ZoneName {
        name: "Asia/Riyadh",
        abbrev: "Asia/Riyadh",
        data: "Asia/Riyadh",
    },
    ZoneName {
        name: "Asia/Saigon",
        abbrev: "VST",
        data: "Asia/Saigon",
    },
    ZoneName {
        name: "Asia/Sakhalin",
        abbrev: "Asia/Sakhalin",
        data: "Asia/Sakhalin",
    },
    ZoneName {
        name: "Asia/Samarkand",
        abbrev: "Asia/Samarkand",
        data: "Asia/Samarkand",
    },
    ZoneName {
        name: "Asia/Seoul",
        abbrev: "ROK",
        data: "Asia/Seoul",
    },
    ZoneName {
        name: "Asia/Shanghai",
        abbrev: "CTT",
        data: "Asia/Shanghai",
    },
    ZoneName {
        name: "Asia/Singapore",
        abbrev: "Singapore",
        data: "Asia/Singapore",
    },
    ZoneName {
        name: "Asia/Srednekolymsk",
        abbrev: "Asia/Srednekolymsk",
        data: "Asia/Srednekolymsk",
    },
    ZoneName {
        name: "Asia/Taipei",
        abbrev: "ROC",
        data: "Asia/Taipei",
    },
    ZoneName {
        name: "Asia/Tashkent",
        abbrev: "Asia/Tashkent",
        data: "Asia/Tashkent",
    },
    ZoneName {
        name: "Asia/Tbilisi",
        abbrev: "Asia/Tbilisi",
        data: "Asia/Tbilisi",
    },
    ZoneName {
        name: "Asia/Tehran",
        abbrev: "Iran",
        data: "Asia/Tehran",
    },
    ZoneName {
        name: "Asia/Tel_Aviv",
        abbrev: "Israel",
        data: "Asia/Tel_Aviv",
    },
    ZoneName {
        name: "Asia/Thimbu",
        abbrev: "Asia/Thimbu",
        data: "Asia/Thimbu",
    },
    ZoneName {
        name: "Asia/Thimphu",
        abbrev: "Asia/Thimphu",
        data: "Asia/Thimphu",
    },
    ZoneName {
        name: "Asia/Tokyo",
        abbrev: "JST",
        data: "Asia/Tokyo",
    },
    ZoneName {
        name: "Asia/Tomsk",
        abbrev: "Asia/Tomsk",
        data: "Asia/Tomsk",
    },
    ZoneName {
        name: "Asia/Ujung_Pandang",
        abbrev: "Asia/Ujung_Pandang",
        data: "Asia/Ujung_Pandang",
    },
    ZoneName {
        name: "Asia/Ulaanbaatar",
        abbrev: "Asia/Ulaanbaatar",
        data: "Asia/Ulaanbaatar",
    },
    ZoneName {
        name: "Asia/Ulan_Bator",
        abbrev: "Asia/Ulan_Bator",
        data: "Asia/Ulan_Bator",
    },
    ZoneName {
        name: "Asia/Urumqi",
        abbrev: "Asia/Urumqi",
        data: "Asia/Urumqi",
    },
    ZoneName {
        name: "Asia/Ust-Nera",
        abbrev: "Asia/Ust-Nera",
        data: "Asia/Ust-Nera",
    },
    ZoneName {
        name: "Asia/Vientiane",
        abbrev: "Asia/Vientiane",
        data: "Asia/Vientiane",
    },
    ZoneName {
        name: "Asia/Vladivostok",
        abbrev: "Asia/Vladivostok",
        data: "Asia/Vladivostok",
    },
    ZoneName {
        name: "Asia/Yakutsk",
        abbrev: "Asia/Yakutsk",
        data: "Asia/Yakutsk",
    },
    ZoneName {
        name: "Asia/Yangon",
        abbrev: "Asia/Yangon",
        data: "Asia/Yangon",
    },
    ZoneName {
        name: "Asia/Yekaterinburg",
        abbrev: "Asia/Yekaterinburg",
        data: "Asia/Yekaterinburg",
    },
    ZoneName {
        name: "Asia/Yerevan",
        abbrev: "NET",
        data: "Asia/Yerevan",
    },
    ZoneName {
        name: "Atlantic/Azores",
        abbrev: "Atlantic/Azores",
        data: "Atlantic/Azores",
    },
    ZoneName {
        name: "Atlantic/Bermuda",
        abbrev: "Atlantic/Bermuda",
        data: "Atlantic/Bermuda",
    },
    ZoneName {
        name: "Atlantic/Canary",
        abbrev: "Atlantic/Canary",
        data: "Atlantic/Canary",
    },
    ZoneName {
        name: "Atlantic/Cape_Verde",
        abbrev: "Atlantic/Cape_Verde",
        data: "Atlantic/Cape_Verde",
    },
    ZoneName {
        name: "Atlantic/Faeroe",
        abbrev: "Atlantic/Faeroe",
        data: "Atlantic/Faeroe",
    },
    ZoneName {
        name: "Atlantic/Faroe",
        abbrev: "Atlantic/Faroe",
        data: "Atlantic/Faroe",
    },
    ZoneName {
        name: "Atlantic/Jan_Mayen",
        abbrev: "Atlantic/Jan_Mayen",
        data: "Atlantic/Jan_Mayen",
    },
    ZoneName {
        name: "Atlantic/Madeira",
        abbrev: "Atlantic/Madeira",
        data: "Atlantic/Madeira",
    },
    ZoneName {
        name: "Atlantic/Reykjavik",
        abbrev: "Iceland",
        data: "Atlantic/Reykjavik",
    },
    ZoneName {
        name: "Atlantic/South_Georgia",
        abbrev: "Atlantic/South_Georgia",
        data: "Atlantic/South_Georgia",
    },
    ZoneName {
        name: "Atlantic/St_Helena",
        abbrev: "Iceland",
        data: "Atlantic/St_Helena",
    },
    ZoneName {
        name: "Atlantic/Stanley",
        abbrev: "Atlantic/Stanley",
        data: "Atlantic/Stanley",
    },
    ZoneName {
        name: "Australia/ACT",
        abbrev: "AET",
        data: "Australia/ACT",
    },
    ZoneName {
        name: "Australia/Adelaide",
        abbrev: "Australia/Adelaide",
        data: "Australia/Adelaide",
    },
    ZoneName {
        name: "Australia/Brisbane",
        abbrev: "Australia/Brisbane",
        data: "Australia/Brisbane",
    },
    ZoneName {
        name: "Australia/Broken_Hill",
        abbrev: "Australia/Broken_Hill",
        data: "Australia/Broken_Hill",
    },
    ZoneName {
        name: "Australia/Canberra",
        abbrev: "AET",
        data: "Australia/Canberra",
    },
    ZoneName {
        name: "Australia/Currie",
        abbrev: "Australia/Currie",
        data: "Australia/Currie",
    },
    ZoneName {
        name: "Australia/Darwin",
        abbrev: "ACT",
        data: "Australia/Darwin",
    },
    ZoneName {
        name: "Australia/Eucla",
        abbrev: "Australia/Eucla",
        data: "Australia/Eucla",
    },
    ZoneName {
        name: "Australia/Hobart",
        abbrev: "Australia/Hobart",
        data: "Australia/Hobart",
    },
    ZoneName {
        name: "Australia/LHI",
        abbrev: "Australia/LHI",
        data: "Australia/LHI",
    },
    ZoneName {
        name: "Australia/Lindeman",
        abbrev: "Australia/Lindeman",
        data: "Australia/Lindeman",
    },
    ZoneName {
        name: "Australia/Lord_Howe",
        abbrev: "Australia/Lord_Howe",
        data: "Australia/Lord_Howe",
    },
    ZoneName {
        name: "Australia/Melbourne",
        abbrev: "Australia/Melbourne",
        data: "Australia/Melbourne",
    },
    ZoneName {
        name: "Australia/NSW",
        abbrev: "AET",
        data: "Australia/NSW",
    },
    ZoneName {
        name: "Australia/North",
        abbrev: "ACT",
        data: "Australia/North",
    },
    ZoneName {
        name: "Australia/Perth",
        abbrev: "Australia/Perth",
        data: "Australia/Perth",
    },
    ZoneName {
        name: "Australia/Queensland",
        abbrev: "Australia/Queensland",
        data: "Australia/Queensland",
    },
    ZoneName {
        name: "Australia/South",
        abbrev: "Australia/South",
        data: "Australia/South",
    },
    ZoneName {
        name: "Australia/Sydney",
        abbrev: "AET",
        data: "Australia/Sydney",
    },
    ZoneName {
        name: "Australia/Tasmania",
        abbrev: "Australia/Tasmania",
        data: "Australia/Tasmania",
    },
    ZoneName {
        name: "Australia/Victoria",
        abbrev: "Australia/Victoria",
        data: "Australia/Victoria",
    },
    ZoneName {
        name: "Australia/West",
        abbrev: "Australia/West",
        data: "Australia/West",
    },
    ZoneName {
        name: "Australia/Yancowinna",
        abbrev: "Australia/Yancowinna",
        data: "Australia/Yancowinna",
    },
    ZoneName {
        name: "BET",
        abbrev: "BET",
        data: "America/Sao_Paulo",
    },
    ZoneName {
        name: "BST",
        abbrev: "BST",
        data: "Asia/Dhaka",
    },
    ZoneName {
        name: "Brazil/Acre",
        abbrev: "Brazil/Acre",
        data: "Brazil/Acre",
    },
    ZoneName {
        name: "Brazil/DeNoronha",
        abbrev: "Brazil/DeNoronha",
        data: "Brazil/DeNoronha",
    },
    ZoneName {
        name: "Brazil/East",
        abbrev: "BET",
        data: "Brazil/East",
    },
    ZoneName {
        name: "Brazil/West",
        abbrev: "Brazil/West",
        data: "Brazil/West",
    },
    ZoneName {
        name: "CAT",
        abbrev: "CAT",
        data: "Africa/Maputo",
    },
    ZoneName {
        name: "CET",
        abbrev: "CET",
        data: "CET",
    },
    ZoneName {
        name: "CNT",
        abbrev: "CNT",
        data: "America/St_Johns",
    },
    ZoneName {
        name: "CST",
        abbrev: "CST",
        data: "America/Chicago",
    },
    ZoneName {
        name: "CST6CDT",
        abbrev: "CST6CDT",
        data: "CST6CDT",
    },
    ZoneName {
        name: "CTT",
        abbrev: "CTT",
        data: "Asia/Shanghai",
    },
    ZoneName {
        name: "Canada/Atlantic",
        abbrev: "Canada/Atlantic",
        data: "Canada/Atlantic",
    },
    ZoneName {
        name: "Canada/Central",
        abbrev: "Canada/Central",
        data: "Canada/Central",
    },
    ZoneName {
        name: "Canada/East-Saskatchewan",
        abbrev: "Canada/East-Saskatchewan",
        data: "America/Regina",
    },
    ZoneName {
        name: "Canada/Eastern",
        abbrev: "Canada/Eastern",
        data: "Canada/Eastern",
    },
    ZoneName {
        name: "Canada/Mountain",
        abbrev: "Canada/Mountain",
        data: "Canada/Mountain",
    },
    ZoneName {
        name: "Canada/Newfoundland",
        abbrev: "CNT",
        data: "Canada/Newfoundland",
    },
    ZoneName {
        name: "Canada/Pacific",
        abbrev: "Canada/Pacific",
        data: "Canada/Pacific",
    },
    ZoneName {
        name: "Canada/Saskatchewan",
        abbrev: "Canada/Saskatchewan",
        data: "Canada/Saskatchewan",
    },
    ZoneName {
        name: "Canada/Yukon",
        abbrev: "Canada/Yukon",
        data: "Canada/Yukon",
    },
    ZoneName {
        name: "Chile/Continental",
        abbrev: "Chile/Continental",
        data: "Chile/Continental",
    },
    ZoneName {
        name: "Chile/EasterIsland",
        abbrev: "Chile/EasterIsland",
        data: "Chile/EasterIsland",
    },
    ZoneName {
        name: "Cuba",
        abbrev: "Cuba",
        data: "Cuba",
    },
    ZoneName {
        name: "EAT",
        abbrev: "EAT",
        data: "Africa/Nairobi",
    },
    ZoneName {
        name: "ECT",
        abbrev: "ECT",
        data: "Europe/Paris",
    },
    ZoneName {
        name: "EET",
        abbrev: "EET",
        data: "EET",
    },
    ZoneName {
        name: "EST",
        abbrev: "EST",
        data: "EST",
    },
    ZoneName {
        name: "EST5EDT",
        abbrev: "EST5EDT",
        data: "EST5EDT",
    },
    ZoneName {
        name: "Egypt",
        abbrev: "ART",
        data: "Egypt",
    },
    ZoneName {
        name: "Eire",
        abbrev: "Eire",
        data: "Eire",
    },
    ZoneName {
        name: "Etc/GMT",
        abbrev: "GMT",
        data: "Etc/GMT",
    },
    ZoneName {
        name: "Etc/GMT+0",
        abbrev: "GMT",
        data: "Etc/GMT+0",
    },
    ZoneName {
        name: "Etc/GMT+1",
        abbrev: "Etc/GMT+1",
        data: "Etc/GMT+1",
    },
    ZoneName {
        name: "Etc/GMT+10",
        abbrev: "Etc/GMT+10",
        data: "Etc/GMT+10",
    },
    ZoneName {
        name: "Etc/GMT+11",
        abbrev: "Etc/GMT+11",
        data: "Etc/GMT+11",
    },
    ZoneName {
        name: "Etc/GMT+12",
        abbrev: "Etc/GMT+12",
        data: "Etc/GMT+12",
    },
    ZoneName {
        name: "Etc/GMT+2",
        abbrev: "Etc/GMT+2",
        data: "Etc/GMT+2",
    },
    ZoneName {
        name: "Etc/GMT+3",
        abbrev: "Etc/GMT+3",
        data: "Etc/GMT+3",
    },
    ZoneName {
        name: "Etc/GMT+4",
        abbrev: "Etc/GMT+4",
        data: "Etc/GMT+4",
    },
    ZoneName {
        name: "Etc/GMT+5",
        abbrev: "Etc/GMT+5",
        data: "Etc/GMT+5",
    },
    ZoneName {
        name: "Etc/GMT+6",
        abbrev: "Etc/GMT+6",
        data: "Etc/GMT+6",
    },
    ZoneName {
        name: "Etc/GMT+7",
        abbrev: "Etc/GMT+7",
        data: "Etc/GMT+7",
    },
    ZoneName {
        name: "Etc/GMT+8",
        abbrev: "Etc/GMT+8",
        data: "Etc/GMT+8",
    },
    ZoneName {
        name: "Etc/GMT+9",
        abbrev: "Etc/GMT+9",
        data: "Etc/GMT+9",
    },
    ZoneName {
        name: "Etc/GMT-0",
        abbrev: "GMT",
        data: "Etc/GMT-0",
    },
    ZoneName {
        name: "Etc/GMT-1",
        abbrev: "Etc/GMT-1",
        data: "Etc/GMT-1",
    },
    ZoneName {
        name: "Etc/GMT-10",
        abbrev: "Etc/GMT-10",
        data: "Etc/GMT-10",
    },
    ZoneName {
        name: "Etc/GMT-11",
        abbrev: "Etc/GMT-11",
        data: "Etc/GMT-11",
    },
    ZoneName {
        name: "Etc/GMT-12",
        abbrev: "Etc/GMT-12",
        data: "Etc/GMT-12",
    },
    ZoneName {
        name: "Etc/GMT-13",
        abbrev: "Etc/GMT-13",
        data: "Etc/GMT-13",
    },
    ZoneName {
        name: "Etc/GMT-14",
        abbrev: "Etc/GMT-14",
        data: "Etc/GMT-14",
    },
    ZoneName {
        name: "Etc/GMT-2",
        abbrev: "Etc/GMT-2",
        data: "Etc/GMT-2",
    },
    ZoneName {
        name: "Etc/GMT-3",
        abbrev: "Etc/GMT-3",
        data: "Etc/GMT-3",
    },
    ZoneName {
        name: "Etc/GMT-4",
        abbrev: "Etc/GMT-4",
        data: "Etc/GMT-4",
    },
    ZoneName {
        name: "Etc/GMT-5",
        abbrev: "Etc/GMT-5",
        data: "Etc/GMT-5",
    },
    ZoneName {
        name: "Etc/GMT-6",
        abbrev: "Etc/GMT-6",
        data: "Etc/GMT-6",
    },
    ZoneName {
        name: "Etc/GMT-7",
        abbrev: "Etc/GMT-7",
        data: "Etc/GMT-7",
    },
    ZoneName {
        name: "Etc/GMT-8",
        abbrev: "Etc/GMT-8",
        data: "Etc/GMT-8",
    },
    ZoneName {
        name: "Etc/GMT-9",
        abbrev: "Etc/GMT-9",
        data: "Etc/GMT-9",
    },
    ZoneName {
        name: "Etc/GMT0",
        abbrev: "GMT",
        data: "Etc/GMT0",
    },
    ZoneName {
        name: "Etc/Greenwich",
        abbrev: "GMT",
        data: "Etc/Greenwich",
    },
    ZoneName {
        name: "Etc/UCT",
        abbrev: "UCT",
        data: "Etc/UCT",
    },
    ZoneName {
        name: "Etc/UTC",
        abbrev: "UCT",
        data: "Etc/UTC",
    },
    ZoneName {
        name: "Etc/Universal",
        abbrev: "UCT",
        data: "Etc/Universal",
    },
    ZoneName {
        name: "Etc/Zulu",
        abbrev: "UCT",
        data: "Etc/Zulu",
    },
    ZoneName {
        name: "Europe/Amsterdam",
        abbrev: "CET",
        data: "Europe/Amsterdam",
    },
    ZoneName {
        name: "Europe/Andorra",
        abbrev: "Europe/Andorra",
        data: "Europe/Andorra",
    },
    ZoneName {
        name: "Europe/Astrakhan",
        abbrev: "Europe/Astrakhan",
        data: "Europe/Astrakhan",
    },
    ZoneName {
        name: "Europe/Athens",
        abbrev: "EET",
        data: "Europe/Athens",
    },
    ZoneName {
        name: "Europe/Belfast",
        abbrev: "GB",
        data: "Europe/Belfast",
    },
    ZoneName {
        name: "Europe/Belgrade",
        abbrev: "Europe/Belgrade",
        data: "Europe/Belgrade",
    },
    ZoneName {
        name: "Europe/Berlin",
        abbrev: "Europe/Berlin",
        data: "Europe/Berlin",
    },
    ZoneName {
        name: "Europe/Bratislava",
        abbrev: "Europe/Bratislava",
        data: "Europe/Bratislava",
    },
    ZoneName {
        name: "Europe/Brussels",
        abbrev: "CET",
        data: "Europe/Brussels",
    },
    ZoneName {
        name: "Europe/Bucharest",
        abbrev: "Europe/Bucharest",
        data: "Europe/Bucharest",
    },
    ZoneName {
        name: "Europe/Budapest",
        abbrev: "Europe/Budapest",
        data: "Europe/Budapest",
    },
    ZoneName {
        name: "Europe/Busingen",
        abbrev: "Europe/Busingen",
        data: "Europe/Busingen",
    },
    ZoneName {
        name: "Europe/Chisinau",
        abbrev: "Europe/Chisinau",
        data: "Europe/Chisinau",
    },
    ZoneName {
        name: "Europe/Copenhagen",
        abbrev: "Europe/Copenhagen",
        data: "Europe/Copenhagen",
    },
    ZoneName {
        name: "Europe/Dublin",
        abbrev: "Eire",
        data: "Europe/Dublin",
    },
    ZoneName {
        name: "Europe/Gibraltar",
        abbrev: "Europe/Gibraltar",
        data: "Europe/Gibraltar",
    },
    ZoneName {
        name: "Europe/Guernsey",
        abbrev: "GB",
        data: "Europe/Guernsey",
    },
    ZoneName {
        name: "Europe/Helsinki",
        abbrev: "Europe/Helsinki",
        data: "Europe/Helsinki",
    },
    ZoneName {
        name: "Europe/Isle_of_Man",
        abbrev: "GB",
        data: "Europe/Isle_of_Man",
    },
    ZoneName {
        name: "Europe/Istanbul",
        abbrev: "Turkey",
        data: "Europe/Istanbul",
    },
    ZoneName {
        name: "Europe/Jersey",
        abbrev: "GB",
        data: "Europe/Jersey",
    },
    ZoneName {
        name: "Europe/Kaliningrad",
        abbrev: "Europe/Kaliningrad",
        data: "Europe/Kaliningrad",
    },
    ZoneName {
        name: "Europe/Kiev",
        abbrev: "Europe/Kiev",
        data: "Europe/Kiev",
    },
    ZoneName {
        name: "Europe/Kirov",
        abbrev: "Europe/Kirov",
        data: "Europe/Kirov",
    },
    ZoneName {
        name: "Europe/Kyiv",
        abbrev: "Europe/Kyiv",
        data: "Europe/Kyiv",
    },
    ZoneName {
        name: "Europe/Lisbon",
        abbrev: "WET",
        data: "Europe/Lisbon",
    },
    ZoneName {
        name: "Europe/Ljubljana",
        abbrev: "Europe/Ljubljana",
        data: "Europe/Ljubljana",
    },
    ZoneName {
        name: "Europe/London",
        abbrev: "GB",
        data: "Europe/London",
    },
    ZoneName {
        name: "Europe/Luxembourg",
        abbrev: "CET",
        data: "Europe/Luxembourg",
    },
    ZoneName {
        name: "Europe/Madrid",
        abbrev: "Europe/Madrid",
        data: "Europe/Madrid",
    },
    ZoneName {
        name: "Europe/Malta",
        abbrev: "Europe/Malta",
        data: "Europe/Malta",
    },
    ZoneName {
        name: "Europe/Mariehamn",
        abbrev: "Europe/Mariehamn",
        data: "Europe/Mariehamn",
    },
    ZoneName {
        name: "Europe/Minsk",
        abbrev: "Europe/Minsk",
        data: "Europe/Minsk",
    },
    ZoneName {
        name: "Europe/Monaco",
        abbrev: "ECT",
        data: "Europe/Monaco",
    },
    ZoneName {
        name: "Europe/Moscow",
        abbrev: "W-SU",
        data: "Europe/Moscow",
    },
    ZoneName {
        name: "Europe/Nicosia",
        abbrev: "Europe/Nicosia",
        data: "Europe/Nicosia",
    },
    ZoneName {
        name: "Europe/Oslo",
        abbrev: "Europe/Oslo",
        data: "Europe/Oslo",
    },
    ZoneName {
        name: "Europe/Paris",
        abbrev: "ECT",
        data: "Europe/Paris",
    },
    ZoneName {
        name: "Europe/Podgorica",
        abbrev: "Europe/Podgorica",
        data: "Europe/Podgorica",
    },
    ZoneName {
        name: "Europe/Prague",
        abbrev: "Europe/Prague",
        data: "Europe/Prague",
    },
    ZoneName {
        name: "Europe/Riga",
        abbrev: "Europe/Riga",
        data: "Europe/Riga",
    },
    ZoneName {
        name: "Europe/Rome",
        abbrev: "Europe/Rome",
        data: "Europe/Rome",
    },
    ZoneName {
        name: "Europe/Samara",
        abbrev: "Europe/Samara",
        data: "Europe/Samara",
    },
    ZoneName {
        name: "Europe/San_Marino",
        abbrev: "Europe/San_Marino",
        data: "Europe/San_Marino",
    },
    ZoneName {
        name: "Europe/Sarajevo",
        abbrev: "Europe/Sarajevo",
        data: "Europe/Sarajevo",
    },
    ZoneName {
        name: "Europe/Saratov",
        abbrev: "Europe/Saratov",
        data: "Europe/Saratov",
    },
    ZoneName {
        name: "Europe/Simferopol",
        abbrev: "Europe/Simferopol",
        data: "Europe/Simferopol",
    },
    ZoneName {
        name: "Europe/Skopje",
        abbrev: "Europe/Skopje",
        data: "Europe/Skopje",
    },
    ZoneName {
        name: "Europe/Sofia",
        abbrev: "Europe/Sofia",
        data: "Europe/Sofia",
    },
    ZoneName {
        name: "Europe/Stockholm",
        abbrev: "Europe/Stockholm",
        data: "Europe/Stockholm",
    },
    ZoneName {
        name: "Europe/Tallinn",
        abbrev: "Europe/Tallinn",
        data: "Europe/Tallinn",
    },
    ZoneName {
        name: "Europe/Tirane",
        abbrev: "Europe/Tirane",
        data: "Europe/Tirane",
    },
    ZoneName {
        name: "Europe/Tiraspol",
        abbrev: "Europe/Tiraspol",
        data: "Europe/Tiraspol",
    },
    ZoneName {
        name: "Europe/Ulyanovsk",
        abbrev: "Europe/Ulyanovsk",
        data: "Europe/Ulyanovsk",
    },
    ZoneName {
        name: "Europe/Uzhgorod",
        abbrev: "Europe/Uzhgorod",
        data: "Europe/Uzhgorod",
    },
    ZoneName {
        name: "Europe/Vaduz",
        abbrev: "Europe/Vaduz",
        data: "Europe/Vaduz",
    },
    ZoneName {
        name: "Europe/Vatican",
        abbrev: "Europe/Vatican",
        data: "Europe/Vatican",
    },
    ZoneName {
        name: "Europe/Vienna",
        abbrev: "Europe/Vienna",
        data: "Europe/Vienna",
    },
    ZoneName {
        name: "Europe/Vilnius",
        abbrev: "Europe/Vilnius",
        data: "Europe/Vilnius",
    },
    ZoneName {
        name: "Europe/Volgograd",
        abbrev: "Europe/Volgograd",
        data: "Europe/Volgograd",
    },
    ZoneName {
        name: "Europe/Warsaw",
        abbrev: "Poland",
        data: "Europe/Warsaw",
    },
    ZoneName {
        name: "Europe/Zagreb",
        abbrev: "Europe/Zagreb",
        data: "Europe/Zagreb",
    },
    ZoneName {
        name: "Europe/Zaporozhye",
        abbrev: "Europe/Zaporozhye",
        data: "Europe/Zaporozhye",
    },
    ZoneName {
        name: "Europe/Zurich",
        abbrev: "Europe/Zurich",
        data: "Europe/Zurich",
    },
    ZoneName {
        name: "Factory",
        abbrev: "Factory",
        data: "Factory",
    },
    ZoneName {
        name: "GB",
        abbrev: "GB",
        data: "GB",
    },
    ZoneName {
        name: "GB-Eire",
        abbrev: "GB",
        data: "GB-Eire",
    },
    ZoneName {
        name: "GMT",
        abbrev: "GMT",
        data: "GMT",
    },
    ZoneName {
        name: "GMT+0",
        abbrev: "GMT",
        data: "GMT+0",
    },
    ZoneName {
        name: "GMT-0",
        abbrev: "GMT",
        data: "GMT-0",
    },
    ZoneName {
        name: "GMT0",
        abbrev: "GMT",
        data: "GMT0",
    },
    ZoneName {
        name: "Greenwich",
        abbrev: "GMT",
        data: "Greenwich",
    },
    ZoneName {
        name: "HST",
        abbrev: "HST",
        data: "HST",
    },
    ZoneName {
        name: "Hongkong",
        abbrev: "Hongkong",
        data: "Hongkong",
    },
    ZoneName {
        name: "IET",
        abbrev: "IET",
        data: "America/Indiana/Indianapolis",
    },
    ZoneName {
        name: "IST",
        abbrev: "IST",
        data: "Asia/Kolkata",
    },
    ZoneName {
        name: "Iceland",
        abbrev: "Iceland",
        data: "Iceland",
    },
    ZoneName {
        name: "Indian/Antananarivo",
        abbrev: "EAT",
        data: "Indian/Antananarivo",
    },
    ZoneName {
        name: "Indian/Chagos",
        abbrev: "Indian/Chagos",
        data: "Indian/Chagos",
    },
    ZoneName {
        name: "Indian/Christmas",
        abbrev: "Indian/Christmas",
        data: "Indian/Christmas",
    },
    ZoneName {
        name: "Indian/Cocos",
        abbrev: "Indian/Cocos",
        data: "Indian/Cocos",
    },
    ZoneName {
        name: "Indian/Comoro",
        abbrev: "EAT",
        data: "Indian/Comoro",
    },
    ZoneName {
        name: "Indian/Kerguelen",
        abbrev: "Indian/Kerguelen",
        data: "Indian/Kerguelen",
    },
    ZoneName {
        name: "Indian/Mahe",
        abbrev: "Indian/Mahe",
        data: "Indian/Mahe",
    },
    ZoneName {
        name: "Indian/Maldives",
        abbrev: "Indian/Maldives",
        data: "Indian/Maldives",
    },
    ZoneName {
        name: "Indian/Mauritius",
        abbrev: "Indian/Mauritius",
        data: "Indian/Mauritius",
    },
    ZoneName {
        name: "Indian/Mayotte",
        abbrev: "EAT",
        data: "Indian/Mayotte",
    },
    ZoneName {
        name: "Indian/Reunion",
        abbrev: "Indian/Reunion",
        data: "Indian/Reunion",
    },
    ZoneName {
        name: "Iran",
        abbrev: "Iran",
        data: "Iran",
    },
    ZoneName {
        name: "Israel",
        abbrev: "Israel",
        data: "Israel",
    },
    ZoneName {
        name: "JST",
        abbrev: "JST",
        data: "Asia/Tokyo",
    },
    ZoneName {
        name: "Jamaica",
        abbrev: "Jamaica",
        data: "Jamaica",
    },
    ZoneName {
        name: "Japan",
        abbrev: "JST",
        data: "Japan",
    },
    ZoneName {
        name: "Kwajalein",
        abbrev: "Kwajalein",
        data: "Kwajalein",
    },
    ZoneName {
        name: "Libya",
        abbrev: "Libya",
        data: "Libya",
    },
    ZoneName {
        name: "MET",
        abbrev: "CET",
        data: "MET",
    },
    ZoneName {
        name: "MIT",
        abbrev: "MIT",
        data: "Pacific/Apia",
    },
    ZoneName {
        name: "MST",
        abbrev: "MST",
        data: "MST",
    },
    ZoneName {
        name: "MST7MDT",
        abbrev: "MST7MDT",
        data: "MST7MDT",
    },
    ZoneName {
        name: "Mexico/BajaNorte",
        abbrev: "Mexico/BajaNorte",
        data: "Mexico/BajaNorte",
    },
    ZoneName {
        name: "Mexico/BajaSur",
        abbrev: "Mexico/BajaSur",
        data: "Mexico/BajaSur",
    },
    ZoneName {
        name: "Mexico/General",
        abbrev: "Mexico/General",
        data: "Mexico/General",
    },
    ZoneName {
        name: "NET",
        abbrev: "NET",
        data: "Asia/Yerevan",
    },
    ZoneName {
        name: "NST",
        abbrev: "NZ",
        data: "Pacific/Auckland",
    },
    ZoneName {
        name: "NZ",
        abbrev: "NZ",
        data: "NZ",
    },
    ZoneName {
        name: "NZ-CHAT",
        abbrev: "NZ-CHAT",
        data: "NZ-CHAT",
    },
    ZoneName {
        name: "Navajo",
        abbrev: "Navajo",
        data: "Navajo",
    },
    ZoneName {
        name: "PLT",
        abbrev: "PLT",
        data: "Asia/Karachi",
    },
    ZoneName {
        name: "PNT",
        abbrev: "MST",
        data: "America/Phoenix",
    },
    ZoneName {
        name: "PRC",
        abbrev: "CTT",
        data: "PRC",
    },
    ZoneName {
        name: "PRT",
        abbrev: "PRT",
        data: "America/Puerto_Rico",
    },
    ZoneName {
        name: "PST",
        abbrev: "PST",
        data: "America/Los_Angeles",
    },
    ZoneName {
        name: "PST8PDT",
        abbrev: "PST8PDT",
        data: "PST8PDT",
    },
    ZoneName {
        name: "Pacific/Apia",
        abbrev: "MIT",
        data: "Pacific/Apia",
    },
    ZoneName {
        name: "Pacific/Auckland",
        abbrev: "NZ",
        data: "Pacific/Auckland",
    },
    ZoneName {
        name: "Pacific/Bougainville",
        abbrev: "Pacific/Bougainville",
        data: "Pacific/Bougainville",
    },
    ZoneName {
        name: "Pacific/Chatham",
        abbrev: "NZ-CHAT",
        data: "Pacific/Chatham",
    },
    ZoneName {
        name: "Pacific/Chuuk",
        abbrev: "Pacific/Chuuk",
        data: "Pacific/Chuuk",
    },
    ZoneName {
        name: "Pacific/Easter",
        abbrev: "Pacific/Easter",
        data: "Pacific/Easter",
    },
    ZoneName {
        name: "Pacific/Efate",
        abbrev: "Pacific/Efate",
        data: "Pacific/Efate",
    },
    ZoneName {
        name: "Pacific/Enderbury",
        abbrev: "Pacific/Enderbury",
        data: "Pacific/Enderbury",
    },
    ZoneName {
        name: "Pacific/Fakaofo",
        abbrev: "Pacific/Fakaofo",
        data: "Pacific/Fakaofo",
    },
    ZoneName {
        name: "Pacific/Fiji",
        abbrev: "Pacific/Fiji",
        data: "Pacific/Fiji",
    },
    ZoneName {
        name: "Pacific/Funafuti",
        abbrev: "Pacific/Funafuti",
        data: "Pacific/Funafuti",
    },
    ZoneName {
        name: "Pacific/Galapagos",
        abbrev: "Pacific/Galapagos",
        data: "Pacific/Galapagos",
    },
    ZoneName {
        name: "Pacific/Gambier",
        abbrev: "Pacific/Gambier",
        data: "Pacific/Gambier",
    },
    ZoneName {
        name: "Pacific/Guadalcanal",
        abbrev: "SST",
        data: "Pacific/Guadalcanal",
    },
    ZoneName {
        name: "Pacific/Guam",
        abbrev: "Pacific/Guam",
        data: "Pacific/Guam",
    },
    ZoneName {
        name: "Pacific/Honolulu",
        abbrev: "HST",
        data: "Pacific/Honolulu",
    },
    ZoneName {
        name: "Pacific/Johnston",
        abbrev: "HST",
        data: "Pacific/Johnston",
    },
    ZoneName {
        name: "Pacific/Kanton",
        abbrev: "Pacific/Kanton",
        data: "Pacific/Kanton",
    },
    ZoneName {
        name: "Pacific/Kiritimati",
        abbrev: "Pacific/Kiritimati",
        data: "Pacific/Kiritimati",
    },
    ZoneName {
        name: "Pacific/Kosrae",
        abbrev: "Pacific/Kosrae",
        data: "Pacific/Kosrae",
    },
    ZoneName {
        name: "Pacific/Kwajalein",
        abbrev: "Kwajalein",
        data: "Pacific/Kwajalein",
    },
    ZoneName {
        name: "Pacific/Majuro",
        abbrev: "Pacific/Majuro",
        data: "Pacific/Majuro",
    },
    ZoneName {
        name: "Pacific/Marquesas",
        abbrev: "Pacific/Marquesas",
        data: "Pacific/Marquesas",
    },
    ZoneName {
        name: "Pacific/Midway",
        abbrev: "Pacific/Midway",
        data: "Pacific/Midway",
    },
    ZoneName {
        name: "Pacific/Nauru",
        abbrev: "Pacific/Nauru",
        data: "Pacific/Nauru",
    },
    ZoneName {
        name: "Pacific/Niue",
        abbrev: "Pacific/Niue",
        data: "Pacific/Niue",
    },
    ZoneName {
        name: "Pacific/Norfolk",
        abbrev: "Pacific/Norfolk",
        data: "Pacific/Norfolk",
    },
    ZoneName {
        name: "Pacific/Noumea",
        abbrev: "Pacific/Noumea",
        data: "Pacific/Noumea",
    },
    ZoneName {
        name: "Pacific/Pago_Pago",
        abbrev: "Pacific/Pago_Pago",
        data: "Pacific/Pago_Pago",
    },
    ZoneName {
        name: "Pacific/Palau",
        abbrev: "Pacific/Palau",
        data: "Pacific/Palau",
    },
    ZoneName {
        name: "Pacific/Pitcairn",
        abbrev: "Pacific/Pitcairn",
        data: "Pacific/Pitcairn",
    },
    ZoneName {
        name: "Pacific/Pohnpei",
        abbrev: "SST",
        data: "Pacific/Pohnpei",
    },
    ZoneName {
        name: "Pacific/Ponape",
        abbrev: "SST",
        data: "Pacific/Ponape",
    },
    ZoneName {
        name: "Pacific/Port_Moresby",
        abbrev: "Pacific/Port_Moresby",
        data: "Pacific/Port_Moresby",
    },
    ZoneName {
        name: "Pacific/Rarotonga",
        abbrev: "Pacific/Rarotonga",
        data: "Pacific/Rarotonga",
    },
    ZoneName {
        name: "Pacific/Saipan",
        abbrev: "Pacific/Saipan",
        data: "Pacific/Saipan",
    },
    ZoneName {
        name: "Pacific/Samoa",
        abbrev: "Pacific/Samoa",
        data: "Pacific/Samoa",
    },
    ZoneName {
        name: "Pacific/Tahiti",
        abbrev: "Pacific/Tahiti",
        data: "Pacific/Tahiti",
    },
    ZoneName {
        name: "Pacific/Tarawa",
        abbrev: "Pacific/Tarawa",
        data: "Pacific/Tarawa",
    },
    ZoneName {
        name: "Pacific/Tongatapu",
        abbrev: "Pacific/Tongatapu",
        data: "Pacific/Tongatapu",
    },
    ZoneName {
        name: "Pacific/Truk",
        abbrev: "Pacific/Truk",
        data: "Pacific/Truk",
    },
    ZoneName {
        name: "Pacific/Wake",
        abbrev: "Pacific/Wake",
        data: "Pacific/Wake",
    },
    ZoneName {
        name: "Pacific/Wallis",
        abbrev: "Pacific/Wallis",
        data: "Pacific/Wallis",
    },
    ZoneName {
        name: "Pacific/Yap",
        abbrev: "Pacific/Yap",
        data: "Pacific/Yap",
    },
    ZoneName {
        name: "Poland",
        abbrev: "Poland",
        data: "Poland",
    },
    ZoneName {
        name: "Portugal",
        abbrev: "WET",
        data: "Portugal",
    },
    ZoneName {
        name: "ROC",
        abbrev: "ROC",
        data: "ROC",
    },
    ZoneName {
        name: "ROK",
        abbrev: "ROK",
        data: "ROK",
    },
    ZoneName {
        name: "SST",
        abbrev: "SST",
        data: "Pacific/Guadalcanal",
    },
    ZoneName {
        name: "Singapore",
        abbrev: "Singapore",
        data: "Singapore",
    },
    ZoneName {
        name: "SystemV/AST4",
        abbrev: "SystemV/AST4",
        data: "Etc/GMT+4",
    },
    ZoneName {
        name: "SystemV/AST4ADT",
        abbrev: "SystemV/AST4ADT",
        data: "posix:AST4ADT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "SystemV/CST6",
        abbrev: "SystemV/CST6",
        data: "Etc/GMT+6",
    },
    ZoneName {
        name: "SystemV/CST6CDT",
        abbrev: "SystemV/CST6CDT",
        data: "posix:CST6CDT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "SystemV/EST5",
        abbrev: "SystemV/EST5",
        data: "Etc/GMT+5",
    },
    ZoneName {
        name: "SystemV/EST5EDT",
        abbrev: "SystemV/EST5EDT",
        data: "posix:EST5EDT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "SystemV/HST10",
        abbrev: "SystemV/HST10",
        data: "Etc/GMT+10",
    },
    ZoneName {
        name: "SystemV/MST7",
        abbrev: "SystemV/MST7",
        data: "Etc/GMT+7",
    },
    ZoneName {
        name: "SystemV/MST7MDT",
        abbrev: "SystemV/MST7MDT",
        data: "posix:MST7MDT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "SystemV/PST8",
        abbrev: "SystemV/PST8",
        data: "Etc/GMT+8",
    },
    ZoneName {
        name: "SystemV/PST8PDT",
        abbrev: "SystemV/PST8PDT",
        data: "posix:PST8PDT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "SystemV/YST9",
        abbrev: "SystemV/YST9",
        data: "Etc/GMT+9",
    },
    ZoneName {
        name: "SystemV/YST9YDT",
        abbrev: "SystemV/YST9YDT",
        data: "posix:YST9YDT,M4.1.0,M10.5.0",
    },
    ZoneName {
        name: "Turkey",
        abbrev: "Turkey",
        data: "Turkey",
    },
    ZoneName {
        name: "UCT",
        abbrev: "UCT",
        data: "UCT",
    },
    ZoneName {
        name: "US/Alaska",
        abbrev: "AST",
        data: "US/Alaska",
    },
    ZoneName {
        name: "US/Aleutian",
        abbrev: "US/Aleutian",
        data: "US/Aleutian",
    },
    ZoneName {
        name: "US/Arizona",
        abbrev: "MST",
        data: "US/Arizona",
    },
    ZoneName {
        name: "US/Central",
        abbrev: "CST",
        data: "US/Central",
    },
    ZoneName {
        name: "US/East-Indiana",
        abbrev: "IET",
        data: "US/East-Indiana",
    },
    ZoneName {
        name: "US/Eastern",
        abbrev: "US/Eastern",
        data: "US/Eastern",
    },
    ZoneName {
        name: "US/Hawaii",
        abbrev: "HST",
        data: "US/Hawaii",
    },
    ZoneName {
        name: "US/Indiana-Starke",
        abbrev: "US/Indiana-Starke",
        data: "US/Indiana-Starke",
    },
    ZoneName {
        name: "US/Michigan",
        abbrev: "US/Michigan",
        data: "US/Michigan",
    },
    ZoneName {
        name: "US/Mountain",
        abbrev: "Navajo",
        data: "US/Mountain",
    },
    ZoneName {
        name: "US/Pacific",
        abbrev: "PST",
        data: "US/Pacific",
    },
    ZoneName {
        name: "US/Pacific-New",
        abbrev: "PST",
        data: "America/Los_Angeles",
    },
    ZoneName {
        name: "US/Samoa",
        abbrev: "US/Samoa",
        data: "US/Samoa",
    },
    ZoneName {
        name: "UTC",
        abbrev: "UCT",
        data: "UTC",
    },
    ZoneName {
        name: "Universal",
        abbrev: "UCT",
        data: "Universal",
    },
    ZoneName {
        name: "VST",
        abbrev: "VST",
        data: "Asia/Ho_Chi_Minh",
    },
    ZoneName {
        name: "W-SU",
        abbrev: "W-SU",
        data: "W-SU",
    },
    ZoneName {
        name: "WET",
        abbrev: "WET",
        data: "WET",
    },
    ZoneName {
        name: "Zulu",
        abbrev: "UCT",
        data: "Zulu",
    },
];
