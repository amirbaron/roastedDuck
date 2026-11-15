//! Version/vendor rules from the pinned PostgreSQL extension, not `server_version_num`.
use roastedduck_function::FunctionClient;
use roastedduck_postgres::Session;

const QUERY: &str = "SELECT version(), (SELECT COUNT(*) FROM pg_settings WHERE name LIKE 'rds%')";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Vendor {
    #[default]
    Unknown,
    Postgres,
    Aurora,
    Redshift,
}
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Version {
    parts: [u64; 3],
    vendor: Vendor,
}
impl Version {
    pub(super) fn read(session: &Session, client: &std::sync::Arc<FunctionClient>) -> Self {
        let result = super::logging::query(client, QUERY, || {
            session.text_query(QUERY, client.interrupt_flag().as_deref())
        });
        let Ok(result) = result else {
            return Self::default();
        };
        let Some(row) = result.rows.first() else {
            return Self::default();
        };
        let text = row.first().and_then(Option::as_deref).unwrap_or_default();
        let rds = row
            .get(1)
            .and_then(Option::as_deref)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        Self::parse(text, rds)
    }
    fn parse(text: &str, rds: i64) -> Self {
        let mut parts = [0_u64; 3];
        let mut bytes = text.bytes().skip_while(|b| !b.is_ascii_digit()).peekable();
        for part in &mut parts {
            if !bytes.peek().is_some_and(u8::is_ascii_digit) {
                break;
            }
            while let Some(b) = bytes.next_if(u8::is_ascii_digit) {
                // Upstream strtoll saturates at the signed maximum on overflow.
                *part = (*part)
                    .saturating_mul(10_u64)
                    .saturating_add(u64::from(b - b'0'))
                    .min(i64::MAX as u64);
            }
            if bytes.next() != Some(b'.') {
                break;
            }
        }
        let vendor = if text.contains("Redshift") {
            Vendor::Redshift
        } else if rds > 0 {
            Vendor::Aurora
        } else if text.contains("PostgreSQL") {
            Vendor::Postgres
        } else {
            Vendor::Unknown
        };
        Self { parts, vendor }
    }
    pub(super) fn major(self) -> u64 {
        self.parts[0]
    }
    pub(super) fn before(self, major: u64, minor: u64) -> bool {
        self.parts < [major, minor, 0]
    }
    pub(super) fn redshift(self) -> bool {
        self.vendor == Vendor::Redshift
    }
}
