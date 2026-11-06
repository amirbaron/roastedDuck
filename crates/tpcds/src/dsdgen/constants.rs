//! The constants of the data set (`constants.h`) and the aliases of the `calendar`
//! distribution's sets (`tpcds.idx.h`).

/// `DATA_START_DATE`: the earliest date in the data set.
pub(crate) const DATA_START_DATE: &str = "1998-01-01";
/// `DATA_END_DATE`: the latest date in the data set.
pub(crate) const DATA_END_DATE: &str = "2003-12-31";

/// `CC_EMPLOYEE_MAX`: rises with the square of the scale.
pub(crate) const CC_EMPLOYEE_MAX: i32 = 7;

/// `CP_CATALOGS_PER_YEAR`.
pub(crate) const CP_CATALOGS_PER_YEAR: i32 = 18;

/// `CR_RETURN_PCT`: the percentage of catalog sales that are returned.
pub(crate) const CR_RETURN_PCT: i32 = 10;

/// `CS_QUANTITY_MAX`.
pub(crate) const CS_QUANTITY_MAX: &str = "100";
/// `CS_MARKUP_MAX`.
pub(crate) const CS_MARKUP_MAX: &str = "2.00";
/// `CS_DISCOUNT_MAX`.
pub(crate) const CS_DISCOUNT_MAX: &str = "1.00";
/// `CS_WHOLESALE_MAX`.
pub(crate) const CS_WHOLESALE_MAX: &str = "100.00";
/// `CS_MIN_SHIP_DELAY`: the minimum days from order to ship.
pub(crate) const CS_MIN_SHIP_DELAY: i32 = 2;
/// `CS_MAX_SHIP_DELAY`: the maximum days from order to ship.
pub(crate) const CS_MAX_SHIP_DELAY: i32 = 90;
/// `CS_GIFT_PCT`: the percentage of orders shipped to someone else.
pub(crate) const CS_GIFT_PCT: i32 = 10;

/// `CURRENT_YEAR`: the benchmark's sense of "today".
pub(crate) const CURRENT_YEAR: i32 = 2003;
/// `CURRENT_MONTH`.
pub(crate) const CURRENT_MONTH: i32 = 1;
/// `CURRENT_DAY`.
pub(crate) const CURRENT_DAY: i32 = 8;
/// `CURRENT_QUARTER`.
pub(crate) const CURRENT_QUARTER: i32 = 1;
/// `CURRENT_WEEK`.
pub(crate) const CURRENT_WEEK: i32 = 2;
/// `DATE_MINIMUM`.
pub(crate) const DATE_MINIMUM: &str = "1998-01-01";
/// `DATE_MAXIMUM`.
pub(crate) const DATE_MAXIMUM: &str = "2002-12-31";
/// `YEAR_MINIMUM`.
pub(crate) const YEAR_MINIMUM: i32 = 1998;
/// `YEAR_MAXIMUM`.
pub(crate) const YEAR_MAXIMUM: i32 = 2002;
/// `TODAYS_DATE`.
pub(crate) const TODAYS_DATE: &str = "2003-01-08";

/// `INV_QUANTITY_MIN`.
pub(crate) const INV_QUANTITY_MIN: i32 = 0;
/// `INV_QUANTITY_MAX`.
pub(crate) const INV_QUANTITY_MAX: i32 = 1000;

/// `PROMO_START_MIN`.
pub(crate) const PROMO_START_MIN: i32 = -720;
/// `PROMO_START_MAX`.
pub(crate) const PROMO_START_MAX: i32 = 100;
/// `PROMO_START_MEAN`.
pub(crate) const PROMO_START_MEAN: i32 = 0;
/// `PROMO_LEN_MIN`.
pub(crate) const PROMO_LEN_MIN: i32 = 1;
/// `PROMO_LEN_MAX`.
pub(crate) const PROMO_LEN_MAX: i32 = 60;
/// `PROMO_LEN_MEAN`.
pub(crate) const PROMO_LEN_MEAN: i32 = 0;
/// `PROMO_DETAIL_LEN_MIN`.
pub(crate) const PROMO_DETAIL_LEN_MIN: i32 = 20;
/// `PROMO_DETAIL_LEN_MAX`.
pub(crate) const PROMO_DETAIL_LEN_MAX: i32 = 60;
/// `PROMO_NAME_LEN`.
pub(crate) const PROMO_NAME_LEN: usize = 5;

/// `SR_RETURN_PCT`: the percentage of store sales that are returned.
pub(crate) const SR_RETURN_PCT: i32 = 10;

/// `SS_MIN_SHIP_DELAY`.
pub(crate) const SS_MIN_SHIP_DELAY: i32 = 2;
/// `SS_MAX_SHIP_DELAY`.
pub(crate) const SS_MAX_SHIP_DELAY: i32 = 90;
/// `SS_QUANTITY_MAX`.
pub(crate) const SS_QUANTITY_MAX: &str = "100";
/// `SS_MARKUP_MAX`.
pub(crate) const SS_MARKUP_MAX: &str = "1.00";
/// `SS_DISCOUNT_MAX`.
pub(crate) const SS_DISCOUNT_MAX: &str = "1.00";
/// `SS_WHOLESALE_MAX`.
pub(crate) const SS_WHOLESALE_MAX: &str = "100.00";

/// `WP_AUTOGEN_PCT`.
pub(crate) const WP_AUTOGEN_PCT: i32 = 30;
/// `WP_LINK_MIN`.
pub(crate) const WP_LINK_MIN: i32 = 2;
/// `WP_LINK_MAX`.
pub(crate) const WP_LINK_MAX: i32 = 25;
/// `WP_IMAGE_MIN`.
pub(crate) const WP_IMAGE_MIN: i32 = 1;
/// `WP_IMAGE_MAX`.
pub(crate) const WP_IMAGE_MAX: i32 = 7;
/// `WP_AD_MIN`.
pub(crate) const WP_AD_MIN: i32 = 0;
/// `WP_AD_MAX`.
pub(crate) const WP_AD_MAX: i32 = 4;
/// `WP_IDLE_TIME_MAX`: the maximum time since the last page access.
pub(crate) const WP_IDLE_TIME_MAX: i32 = 100;

/// `W_SQFT_MIN`.
pub(crate) const W_SQFT_MIN: i32 = 50000;
/// `W_SQFT_MAX`.
pub(crate) const W_SQFT_MAX: i32 = 1_000_000;
/// `W_NAME_MIN`.
pub(crate) const W_NAME_MIN: i32 = 10;

/// `WR_RETURN_PCT`: the percentage of web sales that are returned.
pub(crate) const WR_RETURN_PCT: i32 = 10;

/// `WEB_START_DATE`: the range of open and close dates of the web sites.
pub(crate) const WEB_START_DATE: &str = DATE_MINIMUM;
/// `WEB_END_DATE`.
pub(crate) const WEB_END_DATE: &str = DATE_MAXIMUM;
/// `WEB_DATE_STAGGER`: the time between site creations.
pub(crate) const WEB_DATE_STAGGER: i64 = 17;
/// `WEB_PAGES_PER_SITE`.
pub(crate) const WEB_PAGES_PER_SITE: i32 = 123;
/// `WEB_MORTALITY`: the percentage of sites that "die" between start and end.
const WEB_MORTALITY: i64 = 50;

/// `WEB_IS_REPLACED`: whether web site `j` is replaced.
pub(crate) const fn web_is_replaced(j: i64) -> bool {
    j % (100 / WEB_MORTALITY) == 0
}

/// `WEB_IS_REPLACEMENT`: whether web site `j` is the replacement.
pub(crate) const fn web_is_replacement(j: i64) -> bool {
    (j / (100 / WEB_MORTALITY)) % 2 != 0
}

/// `RS_CC_MARKET_CLASS`.
pub(crate) const RS_CC_MARKET_CLASS: i32 = 50;
/// `RS_CC_MARKET_DESC`.
pub(crate) const RS_CC_MARKET_DESC: i32 = 100;
/// `RS_CC_DIVISION_NAME`.
pub(crate) const RS_CC_DIVISION_NAME: usize = 50;
/// `RS_CP_DESCRIPTION`.
pub(crate) const RS_CP_DESCRIPTION: i32 = 100;
/// `RS_I_ITEM_DESC`.
pub(crate) const RS_I_ITEM_DESC: i32 = 200;
/// `RS_I_MANUFACT`.
pub(crate) const RS_I_MANUFACT: usize = 50;
/// `RS_I_FORMULATION`.
pub(crate) const RS_I_FORMULATION: i32 = 20;
/// `RS_I_PRODUCT_NAME`.
pub(crate) const RS_I_PRODUCT_NAME: usize = 50;
/// `RS_S_MARKET_DESC`.
pub(crate) const RS_S_MARKET_DESC: i32 = 100;
/// `RS_SM_CONTRACT`.
pub(crate) const RS_SM_CONTRACT: i32 = 20;
/// `RS_W_WAREHOUSE_NAME`.
pub(crate) const RS_W_WAREHOUSE_NAME: i32 = 20;
/// `RS_WEB_MARKET_CLASS`.
pub(crate) const RS_WEB_MARKET_CLASS: i32 = 50;
/// `RS_WEB_MARKET_DESC`.
pub(crate) const RS_WEB_MARKET_DESC: i32 = 100;
/// `RS_WEB_COMPANY_NAME`.
pub(crate) const RS_WEB_COMPANY_NAME: usize = 100;

/// `calendar_uniform`: the uniform weights of the `calendar` distribution.
pub(crate) const CALENDAR_UNIFORM: usize = 1;
/// `calendar_sales`.
pub(crate) const CALENDAR_SALES: usize = 3;
