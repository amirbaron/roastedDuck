//! The general definitions and control information of the generator (DuckDB's
//! `dbgen/dss.h`, `dbgen/shared.h` and the row types of `dbgen/dsstypes.h`): table
//! numbers, column limits, the random number streams and the per-generator state.

/// The tables (`PART` ... `REGION` of `dss.h`); `OrderLine` and `PartPsupp` generate a
/// master table and its detail table together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Table {
    Part = 0,
    Psupp = 1,
    Supp = 2,
    Cust = 3,
    Order = 4,
    Line = 5,
    OrderLine = 6,
    PartPsupp = 7,
    Nation = 8,
    Region = 9,
}

impl Table {
    /// Every table, by number (`PART` ... `REGION`).
    pub(crate) const ALL: [Self; 10] = [
        Self::Part,
        Self::Psupp,
        Self::Supp,
        Self::Cust,
        Self::Order,
        Self::Line,
        Self::OrderLine,
        Self::PartPsupp,
        Self::Nation,
        Self::Region,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

/// `SUPP_PER_PART`.
pub(crate) const SUPP_PER_PART: usize = 4;
/// `ORDERS_PER_CUST`.
pub(crate) const ORDERS_PER_CUST: i64 = 10;
/// `CUST_MORTALITY`: one customer in this many has no orders.
pub(crate) const CUST_MORTALITY: i64 = 3;
/// `NATIONS_MAX`: limited by the country codes of phone numbers.
pub(crate) const NATIONS_MAX: i64 = 90;
/// `STARTDATE`, `CURRENTDATE` and `TOTDATE` (dates are `yyddd`).
pub(crate) const STARTDATE: i64 = 92001;
pub(crate) const CURRENTDATE: i64 = 95168;
pub(crate) const TOTDATE: i64 = 2557;
/// `MAX_STREAM`: the last random number stream.
pub(crate) const MAX_STREAM: usize = 47;
/// `V_STR_LOW` / `V_STR_HGH`: the bounds of variable-length strings relative to their
/// average length.
pub(crate) const V_STR_LOW: f64 = 0.4;
pub(crate) const V_STR_HGH: f64 = 1.6;
/// `PENNIES`: scaled integer money arithmetic.
pub(crate) const PENNIES: i64 = 100;
/// `MIN_SCALE`: scale factors below it shrink the tables instead.
pub(crate) const MIN_SCALE: f64 = 1.0;
/// `MAX_CHILDREN`.
pub(crate) const MAX_CHILDREN: usize = 1000;
/// `MAX_LONG` (`BITS_PER_LONG` is 32).
pub(crate) const MAX_LONG: i64 = 0x7FFF_FFFF;

// The column limits of `dss.h` and `shared.h`: t_ccc_xxx with t a table, ccc a column
// and xxx a limit.
pub(crate) const P_NAME_SCL: usize = 5;
pub(crate) const P_MFG_MIN: i64 = 1;
pub(crate) const P_MFG_MAX: i64 = 5;
pub(crate) const P_BRND_MIN: i64 = 1;
pub(crate) const P_BRND_MAX: i64 = 5;
pub(crate) const P_SIZE_MIN: i64 = 1;
pub(crate) const P_SIZE_MAX: i64 = 50;
pub(crate) const P_CMNT_LEN: i32 = 14;
pub(crate) const S_ABAL_MIN: i64 = -99999;
pub(crate) const S_ABAL_MAX: i64 = 999_999;
pub(crate) const S_ADDR_LEN: i32 = 25;
pub(crate) const S_CMNT_LEN: i32 = 63;
pub(crate) const S_CMNT_BBB: i64 = 10;
pub(crate) const BBB_DEADBEATS: i64 = 50;
pub(crate) const BBB_BASE: &[u8] = b"Customer ";
pub(crate) const BBB_COMPLAIN: &[u8] = b"Complaints";
pub(crate) const BBB_COMMEND: &[u8] = b"Recommends";
pub(crate) const BBB_CMNT_LEN: i64 = 19;
pub(crate) const BBB_BASE_LEN: usize = 9;
pub(crate) const PS_CMNT_LEN: i32 = 124;
pub(crate) const PS_SCST_MIN: i64 = 100;
pub(crate) const PS_SCST_MAX: i64 = 100_000;
pub(crate) const PS_QTY_MIN: i64 = 1;
pub(crate) const PS_QTY_MAX: i64 = 9999;
pub(crate) const C_ADDR_LEN: i32 = 25;
pub(crate) const C_CMNT_LEN: i32 = 73;
pub(crate) const C_ABAL_MIN: i64 = -99999;
pub(crate) const C_ABAL_MAX: i64 = 999_999;
pub(crate) const O_CKEY_MIN: i64 = 1;
pub(crate) const O_CLRK_SCL: i64 = 1000;
pub(crate) const O_CMNT_LEN: i32 = 49;
pub(crate) const O_LCNT_MIN: i64 = 1;
pub(crate) const O_LCNT_MAX: usize = 7;
pub(crate) const L_QTY_MIN: i64 = 1;
pub(crate) const L_QTY_MAX: i64 = 50;
pub(crate) const L_TAX_MIN: i64 = 0;
pub(crate) const L_TAX_MAX: i64 = 8;
pub(crate) const L_DCNT_MIN: i64 = 0;
pub(crate) const L_DCNT_MAX: i64 = 10;
pub(crate) const L_PKEY_MIN: i64 = 1;
pub(crate) const L_SDTE_MIN: i64 = 1;
pub(crate) const L_SDTE_MAX: i64 = 121;
pub(crate) const L_CDTE_MIN: i64 = 30;
pub(crate) const L_CDTE_MAX: i64 = 90;
pub(crate) const L_RDTE_MIN: i64 = 1;
pub(crate) const L_RDTE_MAX: i64 = 30;
pub(crate) const L_CMNT_LEN: i32 = 27;
pub(crate) const N_CMNT_LEN: i32 = 72;
pub(crate) const R_CMNT_LEN: i32 = 72;
pub(crate) const O_ODATE_MIN: i64 = STARTDATE;
pub(crate) const O_ODATE_MAX: i64 = STARTDATE + TOTDATE - (L_SDTE_MAX + L_RDTE_MAX) - 1;

/// The random number streams (seed indexes of `dss.h` and `shared.h`), which separate
/// the generation of the individual columns.
pub(crate) mod stream {
    pub(crate) const P_MFG_SD: usize = 0;
    pub(crate) const P_BRND_SD: usize = 1;
    pub(crate) const P_TYPE_SD: usize = 2;
    pub(crate) const P_SIZE_SD: usize = 3;
    pub(crate) const P_CNTR_SD: usize = 4;
    /// The stream the text pool is generated from.
    pub(crate) const TEXT_POOL_SD: usize = 5;
    pub(crate) const P_CMNT_SD: usize = 6;
    pub(crate) const PS_QTY_SD: usize = 7;
    pub(crate) const PS_SCST_SD: usize = 8;
    pub(crate) const PS_CMNT_SD: usize = 9;
    pub(crate) const O_SUPP_SD: usize = 10;
    pub(crate) const O_CLRK_SD: usize = 11;
    pub(crate) const O_CMNT_SD: usize = 12;
    pub(crate) const O_ODATE_SD: usize = 13;
    pub(crate) const L_QTY_SD: usize = 14;
    pub(crate) const L_DCNT_SD: usize = 15;
    pub(crate) const L_TAX_SD: usize = 16;
    pub(crate) const L_SHIP_SD: usize = 17;
    pub(crate) const L_SMODE_SD: usize = 18;
    pub(crate) const L_PKEY_SD: usize = 19;
    pub(crate) const L_SKEY_SD: usize = 20;
    pub(crate) const L_SDTE_SD: usize = 21;
    pub(crate) const L_CDTE_SD: usize = 22;
    pub(crate) const L_RDTE_SD: usize = 23;
    pub(crate) const L_RFLG_SD: usize = 24;
    pub(crate) const L_CMNT_SD: usize = 25;
    pub(crate) const C_ADDR_SD: usize = 26;
    pub(crate) const C_NTRG_SD: usize = 27;
    pub(crate) const C_PHNE_SD: usize = 28;
    pub(crate) const C_ABAL_SD: usize = 29;
    pub(crate) const C_MSEG_SD: usize = 30;
    pub(crate) const C_CMNT_SD: usize = 31;
    pub(crate) const S_ADDR_SD: usize = 32;
    pub(crate) const S_NTRG_SD: usize = 33;
    pub(crate) const S_PHNE_SD: usize = 34;
    pub(crate) const S_ABAL_SD: usize = 35;
    pub(crate) const S_CMNT_SD: usize = 36;
    pub(crate) const P_NAME_SD: usize = 37;
    pub(crate) const O_PRIO_SD: usize = 38;
    pub(crate) const O_CKEY_SD: usize = 40;
    pub(crate) const N_CMNT_SD: usize = 41;
    pub(crate) const R_CMNT_SD: usize = 42;
    pub(crate) const O_LCNT_SD: usize = 43;
    pub(crate) const BBB_JNK_SD: usize = 44;
    pub(crate) const BBB_TYPE_SD: usize = 45;
    pub(crate) const BBB_CMNT_SD: usize = 46;
    pub(crate) const BBB_OFFSET_SD: usize = 47;
}

/// One random number stream (DuckDB's `seed_t`): its table, current value, the draws of
/// the current row and the draws every row is padded to.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seed {
    pub(crate) table: Option<Table>,
    pub(crate) value: i64,
    pub(crate) usage: i64,
    pub(crate) boundary: i64,
}

const fn seed(table: Option<Table>, value: i64, boundary: i64) -> Seed {
    Seed {
        table,
        value,
        usage: 0,
        boundary,
    }
}

/// The initial streams (`DBGenContext::Seed`).
const INITIAL_SEEDS: [Seed; MAX_STREAM + 1] = {
    use Table::{Cust, Line, Nation, Order, Part, Psupp, Region, Supp};
    let lines = O_LCNT_MAX as i64;
    let parts = SUPP_PER_PART as i64;
    [
        seed(Some(Part), 1, 1),                      // P_MFG_SD     0
        seed(Some(Part), 46_831_694, 1),             // P_BRND_SD    1
        seed(Some(Part), 1_841_581_359, 1),          // P_TYPE_SD    2
        seed(Some(Part), 1_193_163_244, 1),          // P_SIZE_SD    3
        seed(Some(Part), 727_633_698, 1),            // P_CNTR_SD    4
        seed(None, 933_588_178, 1),                  // text pregeneration  5
        seed(Some(Part), 804_159_733, 2),            // P_CMNT_SD    6
        seed(Some(Psupp), 1_671_059_989, parts),     // PS_QTY_SD    7
        seed(Some(Psupp), 1_051_288_424, parts),     // PS_SCST_SD   8
        seed(Some(Psupp), 1_961_692_154, parts * 2), // PS_CMNT_SD   9
        seed(Some(Order), 1_227_283_347, 1),         // O_SUPP_SD    10
        seed(Some(Order), 1_171_034_773, 1),         // O_CLRK_SD    11
        seed(Some(Order), 276_090_261, 2),           // O_CMNT_SD    12
        seed(Some(Order), 1_066_728_069, 1),         // O_ODATE_SD   13
        seed(Some(Line), 209_208_115, lines),        // L_QTY_SD     14
        seed(Some(Line), 554_590_007, lines),        // L_DCNT_SD    15
        seed(Some(Line), 721_958_466, lines),        // L_TAX_SD     16
        seed(Some(Line), 1_371_272_478, lines),      // L_SHIP_SD    17
        seed(Some(Line), 675_466_456, lines),        // L_SMODE_SD   18
        seed(Some(Line), 1_808_217_256, lines),      // L_PKEY_SD    19
        seed(Some(Line), 2_095_021_727, lines),      // L_SKEY_SD    20
        seed(Some(Line), 1_769_349_045, lines),      // L_SDTE_SD    21
        seed(Some(Line), 904_914_315, lines),        // L_CDTE_SD    22
        seed(Some(Line), 373_135_028, lines),        // L_RDTE_SD    23
        seed(Some(Line), 717_419_739, lines),        // L_RFLG_SD    24
        seed(Some(Line), 1_095_462_486, lines * 2),  // L_CMNT_SD    25
        seed(Some(Cust), 881_155_353, 9),            // C_ADDR_SD    26
        seed(Some(Cust), 1_489_529_863, 1),          // C_NTRG_SD    27
        seed(Some(Cust), 1_521_138_112, 3),          // C_PHNE_SD    28
        seed(Some(Cust), 298_370_230, 1),            // C_ABAL_SD    29
        seed(Some(Cust), 1_140_279_430, 1),          // C_MSEG_SD    30
        seed(Some(Cust), 1_335_826_707, 2),          // C_CMNT_SD    31
        seed(Some(Supp), 706_178_559, 9),            // S_ADDR_SD    32
        seed(Some(Supp), 110_356_601, 1),            // S_NTRG_SD    33
        seed(Some(Supp), 884_434_366, 3),            // S_PHNE_SD    34
        seed(Some(Supp), 962_338_209, 1),            // S_ABAL_SD    35
        seed(Some(Supp), 1_341_315_363, 2),          // S_CMNT_SD    36
        seed(Some(Part), 709_314_158, 92),           // P_NAME_SD    37
        seed(Some(Order), 591_449_447, 1),           // O_PRIO_SD    38
        seed(Some(Line), 431_918_286, 1),            // HVAR_SD      39
        seed(Some(Order), 851_767_375, 1),           // O_CKEY_SD    40
        seed(Some(Nation), 606_179_079, 2),          // N_CMNT_SD    41
        seed(Some(Region), 1_500_869_201, 2),        // R_CMNT_SD    42
        seed(Some(Order), 1_434_868_289, 1),         // O_LCNT_SD    43
        seed(Some(Supp), 263_032_577, 1),            // BBB offset   44
        seed(Some(Supp), 753_643_799, 1),            // BBB type     45
        seed(Some(Supp), 202_794_285, 1),            // BBB comment  46
        seed(Some(Supp), 715_851_524, 1),            // BBB junk     47
    ]
};

/// The size and dependent table of one table (DuckDB's `tdef`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TableDef {
    /// The row count at scale factor 1.
    pub(crate) base: i64,
    /// The table generated with this one, whose streams advance with its rows.
    pub(crate) child: Option<Table>,
}

const fn tdef(base: i64, child: Option<Table>) -> TableDef {
    TableDef { base, child }
}

/// The initial table definitions (`DBGenContext::tdefs`).
const INITIAL_TDEFS: [TableDef; 10] = [
    tdef(200_000, Some(Table::Psupp)), // part
    tdef(200_000, None),               // partsupp
    tdef(10_000, None),                // supplier
    tdef(150_000, None),               // customer
    tdef(150_000, Some(Table::Line)),  // orders
    tdef(150_000, None),               // lineitem
    tdef(150_000, Some(Table::Line)),  // orders/lineitem
    tdef(200_000, Some(Table::Psupp)), // part/partsupp
    tdef(NATIONS_MAX, None),           // nation
    tdef(NATIONS_MAX, None),           // region
];

/// The state of one generator (DuckDB's `DBGenContext`): its random number streams, the
/// table sizes and the scale factor. Every thread generating rows has its own.
#[derive(Clone, Debug)]
pub(crate) struct DbgenContext {
    pub(crate) seeds: [Seed; MAX_STREAM + 1],
    pub(crate) tdefs: [TableDef; 10],
    pub(crate) scale_factor: i64,
    /// The permutation buffer of `agg_str` (`permute`).
    pub(crate) permute: Vec<usize>,
}

impl Default for DbgenContext {
    fn default() -> Self {
        Self {
            seeds: INITIAL_SEEDS,
            tdefs: INITIAL_TDEFS,
            scale_factor: 1,
            permute: Vec::new(),
        }
    }
}

impl DbgenContext {
    /// `PS_SKEY_MAX`-style bound: the number of suppliers.
    pub(crate) const fn supplier_count(&self) -> i64 {
        self.tdefs[Table::Supp.index()].base * self.scale_factor
    }

    /// `O_CKEY_MAX`: the largest customer key.
    pub(crate) const fn customer_count(&self) -> i64 {
        self.tdefs[Table::Cust.index()].base * self.scale_factor
    }

    /// `L_PKEY_MAX`: the largest part key.
    pub(crate) const fn part_count(&self) -> i64 {
        self.tdefs[Table::Part.index()].base * self.scale_factor
    }
}

/// A customer row (`customer_t`).
#[derive(Debug, Default)]
pub(crate) struct Customer {
    pub(crate) custkey: i64,
    pub(crate) name: Vec<u8>,
    pub(crate) address: Vec<u8>,
    pub(crate) nation_code: i64,
    pub(crate) phone: [u8; 15],
    pub(crate) acctbal: i64,
    pub(crate) mktsegment: &'static [u8],
    pub(crate) comment: &'static [u8],
}

/// A lineitem row (`line_t`); dates are indexes into the generated date range.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LineItem {
    pub(crate) okey: i64,
    pub(crate) partkey: i64,
    pub(crate) suppkey: i64,
    pub(crate) lcnt: i64,
    pub(crate) quantity: i64,
    pub(crate) eprice: i64,
    pub(crate) discount: i64,
    pub(crate) tax: i64,
    pub(crate) rflag: u8,
    pub(crate) lstatus: u8,
    pub(crate) cdate: usize,
    pub(crate) sdate: usize,
    pub(crate) rdate: usize,
    pub(crate) shipinstruct: &'static [u8],
    pub(crate) shipmode: &'static [u8],
    pub(crate) comment: &'static [u8],
}

/// An order row and its lineitems (`order_t`).
#[derive(Debug, Default)]
pub(crate) struct Order {
    pub(crate) okey: i64,
    pub(crate) custkey: i64,
    pub(crate) orderstatus: u8,
    pub(crate) totalprice: i64,
    pub(crate) odate: usize,
    pub(crate) opriority: &'static [u8],
    pub(crate) clerk: Vec<u8>,
    pub(crate) spriority: i32,
    pub(crate) lines: usize,
    pub(crate) comment: &'static [u8],
    pub(crate) l: [LineItem; O_LCNT_MAX],
}

/// A partsupp row (`partsupp_t`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PartSupp {
    pub(crate) partkey: i64,
    pub(crate) suppkey: i64,
    pub(crate) qty: i64,
    pub(crate) scost: i64,
    pub(crate) comment: &'static [u8],
}

/// A part row and its partsupp rows (`part_t`).
#[derive(Debug, Default)]
pub(crate) struct Part {
    pub(crate) partkey: i64,
    pub(crate) name: Vec<u8>,
    pub(crate) mfgr: Vec<u8>,
    pub(crate) brand: Vec<u8>,
    pub(crate) type_: &'static [u8],
    pub(crate) size: i64,
    pub(crate) container: &'static [u8],
    pub(crate) retailprice: i64,
    pub(crate) comment: &'static [u8],
    pub(crate) s: [PartSupp; SUPP_PER_PART],
}

/// A supplier row (`supplier_t`); the comment is copied since complaints and
/// recommendations are written into it.
#[derive(Debug, Default)]
pub(crate) struct Supplier {
    pub(crate) suppkey: i64,
    pub(crate) name: Vec<u8>,
    pub(crate) address: Vec<u8>,
    pub(crate) nation_code: i64,
    pub(crate) phone: [u8; 15],
    pub(crate) acctbal: i64,
    pub(crate) comment: Vec<u8>,
}

/// A nation or region row (`code_t`).
#[expect(clippy::struct_field_names, reason = "the fields of DuckDB's code_t")]
#[derive(Debug, Default)]
pub(crate) struct Code {
    pub(crate) code: i64,
    pub(crate) text: &'static [u8],
    pub(crate) join: i64,
    pub(crate) comment: &'static [u8],
}
