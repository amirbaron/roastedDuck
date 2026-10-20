//! Generating the rows of each table (DuckDB's `dbgen/build.cpp`).

use super::bm_utils::{agg_str, julian, tpch_a_rnd};
use super::dists::distributions;
use super::dss::{
    BBB_BASE, BBB_BASE_LEN, BBB_CMNT_LEN, BBB_COMMEND, BBB_COMPLAIN, BBB_DEADBEATS, C_ABAL_MAX, C_ABAL_MIN,
    C_ADDR_LEN, C_CMNT_LEN, CURRENTDATE, CUST_MORTALITY, Code, Customer, DbgenContext, L_CDTE_MAX,
    L_CDTE_MIN, L_CMNT_LEN, L_DCNT_MAX, L_DCNT_MIN, L_PKEY_MIN, L_QTY_MAX, L_QTY_MIN, L_RDTE_MAX, L_RDTE_MIN,
    L_SDTE_MAX, L_SDTE_MIN, L_TAX_MAX, L_TAX_MIN, N_CMNT_LEN, NATIONS_MAX, O_CKEY_MIN, O_CLRK_SCL,
    O_CMNT_LEN, O_LCNT_MAX, O_LCNT_MIN, O_ODATE_MAX, O_ODATE_MIN, Order, P_BRND_MAX, P_BRND_MIN, P_CMNT_LEN,
    P_MFG_MAX, P_MFG_MIN, P_NAME_SCL, P_SIZE_MAX, P_SIZE_MIN, PENNIES, PS_CMNT_LEN, PS_QTY_MAX, PS_QTY_MIN,
    PS_SCST_MAX, PS_SCST_MIN, Part, R_CMNT_LEN, S_ABAL_MAX, S_ABAL_MIN, S_ADDR_LEN, S_CMNT_BBB, S_CMNT_LEN,
    STARTDATE, SUPP_PER_PART, Seed, Supplier, V_STR_HGH, V_STR_LOW, stream,
};
use super::rnd::{dss_random, dss_random64};
use super::text::text;

/// `V_STR`: a random alphanumeric string of about `avg` characters.
fn v_str(avg: i32, seed: &mut Seed, dest: &mut Vec<u8>) {
    let avg = f64::from(avg);
    tpch_a_rnd((avg * V_STR_LOW) as i32, (avg * V_STR_HGH) as i32, seed, dest);
}

/// Appends `prefix` and `value` with at least `width` digits (`"%s%0<width>ld"`).
fn push_tagged_number(dest: &mut Vec<u8>, prefix: &[u8], value: i64, width: usize) {
    dest.clear();
    dest.extend_from_slice(prefix);
    let mut digits = [0_u8; 20];
    let mut start = digits.len();
    let mut rest = value.unsigned_abs();
    loop {
        start -= 1;
        digits[start] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    let len = digits.len() - start;
    if value < 0 {
        dest.push(b'-');
    }
    dest.resize(
        dest.len() + width.saturating_sub(len + usize::from(value < 0)),
        b'0',
    );
    dest.extend_from_slice(&digits[start..]);
}

/// Writes `value` (below `10^N`) as `N` digits.
fn write_digits<const N: usize>(target: &mut [u8], value: i64) {
    let mut rest = value;
    for byte in target[..N].iter_mut().rev() {
        *byte = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
}

/// `rpb_routine`: the retail price of part `p` in pennies.
pub(crate) const fn rpb_routine(p: i64) -> i64 {
    let mut price = 90000;
    // Limit the contribution to $200.
    price += (p / 10) % 20001;
    price += (p % 1000) * 100;
    price
}

/// `gen_phone`: a phone number with the country code of nation `ind`.
fn gen_phone(ind: i64, seed: &mut Seed) -> [u8; 15] {
    let acode = dss_random(100, 999, seed);
    let exchg = dss_random(100, 999, seed);
    let number = dss_random(1000, 9999, seed);
    let mut target = *b"00-000-000-0000";
    write_digits::<2>(&mut target[0..], 10 + (ind % NATIONS_MAX));
    write_digits::<3>(&mut target[3..], acode);
    write_digits::<3>(&mut target[7..], exchg);
    write_digits::<4>(&mut target[11..], number);
    target
}

/// `PART_SUPP_BRIDGE`: the key of supplier `s` (of `SUPP_PER_PART`) of part `p`.
const fn part_supp_bridge(ctx: &DbgenContext, p: i64, s: i64) -> i64 {
    let tot_scnt = ctx.supplier_count();
    (p + s * (tot_scnt / SUPP_PER_PART as i64 + (p - 1) / tot_scnt)) % tot_scnt + 1
}

/// `mk_cust`: customer `n_cust`.
pub(crate) fn mk_cust(n_cust: i64, c: &mut Customer, ctx: &mut DbgenContext) {
    let dists = distributions();
    c.custkey = n_cust;
    push_tagged_number(&mut c.name, b"Customer#", n_cust, 9);
    v_str(C_ADDR_LEN, &mut ctx.seeds[stream::C_ADDR_SD], &mut c.address);
    let i = dss_random(
        0,
        dists.nations.count() as i64 - 1,
        &mut ctx.seeds[stream::C_NTRG_SD],
    );
    c.nation_code = i;
    c.phone = gen_phone(i, &mut ctx.seeds[stream::C_PHNE_SD]);
    c.acctbal = dss_random(C_ABAL_MIN, C_ABAL_MAX, &mut ctx.seeds[stream::C_ABAL_SD]);
    c.mktsegment = dists.c_mseg.pick(&mut ctx.seeds[stream::C_MSEG_SD]);
    c.comment = text(C_CMNT_LEN, &mut ctx.seeds[stream::C_CMNT_SD]);
}

/// `mk_sparse`: the sparse order key of order `i` (`seq` 0 for the initial load).
const fn mk_sparse(i: i64, seq: i64) -> i64 {
    // SPARSE_BITS 2 and SPARSE_KEEP 3: keep the low three bits, insert the sequence.
    let low_bits = i & ((1 << 3) - 1);
    ((((i >> 3) << 2) + seq) << 3) + low_bits
}

/// `mk_order`: order `index` and its lineitems.
pub(crate) fn mk_order(index: i64, o: &mut Order, ctx: &mut DbgenContext) {
    let dists = distributions();
    // The initial load (`upd_num` 0).
    o.okey = mk_sparse(index, 0);
    let ckey_max = ctx.customer_count();
    let custkey_seed = &mut ctx.seeds[stream::O_CKEY_SD];
    o.custkey = if ctx.scale_factor >= 30000 {
        dss_random64(O_CKEY_MIN, ckey_max, custkey_seed)
    } else {
        dss_random(O_CKEY_MIN, ckey_max, custkey_seed)
    };
    let mut delta = 1;
    while o.custkey % CUST_MORTALITY == 0 {
        o.custkey += delta;
        o.custkey = o.custkey.min(ckey_max);
        delta *= -1;
    }

    let tmp_date = dss_random(O_ODATE_MIN, O_ODATE_MAX, &mut ctx.seeds[stream::O_ODATE_SD]);
    o.odate = (tmp_date - STARTDATE) as usize;

    o.opriority = dists.o_priority.pick(&mut ctx.seeds[stream::O_PRIO_SD]);
    let clk_num = dss_random(
        1,
        (ctx.scale_factor * O_CLRK_SCL).max(O_CLRK_SCL),
        &mut ctx.seeds[stream::O_CLRK_SD],
    );
    push_tagged_number(&mut o.clerk, b"Clerk#", clk_num, 9);
    o.comment = text(O_CMNT_LEN, &mut ctx.seeds[stream::O_CMNT_SD]);
    o.spriority = 0;

    o.totalprice = 0;
    o.orderstatus = b'O';
    let mut ocnt = 0;

    o.lines = dss_random(O_LCNT_MIN, O_LCNT_MAX as i64, &mut ctx.seeds[stream::O_LCNT_SD]) as usize;
    let pkey_max = ctx.part_count();
    for lcnt in 0..o.lines {
        let mut line = o.l[lcnt];
        line.okey = o.okey;
        line.lcnt = lcnt as i64 + 1;
        line.quantity = dss_random(L_QTY_MIN, L_QTY_MAX, &mut ctx.seeds[stream::L_QTY_SD]);
        line.discount = dss_random(L_DCNT_MIN, L_DCNT_MAX, &mut ctx.seeds[stream::L_DCNT_SD]);
        line.tax = dss_random(L_TAX_MIN, L_TAX_MAX, &mut ctx.seeds[stream::L_TAX_SD]);
        line.shipinstruct = dists.l_instruct.pick(&mut ctx.seeds[stream::L_SHIP_SD]);
        line.shipmode = dists.l_smode.pick(&mut ctx.seeds[stream::L_SMODE_SD]);
        line.comment = text(L_CMNT_LEN, &mut ctx.seeds[stream::L_CMNT_SD]);
        let partkey_seed = &mut ctx.seeds[stream::L_PKEY_SD];
        line.partkey = if ctx.scale_factor >= 30000 {
            dss_random64(L_PKEY_MIN, pkey_max, partkey_seed)
        } else {
            dss_random(L_PKEY_MIN, pkey_max, partkey_seed)
        };
        let rprice = rpb_routine(line.partkey);
        let supp_num = dss_random(0, 3, &mut ctx.seeds[stream::L_SKEY_SD]);
        line.suppkey = part_supp_bridge(ctx, line.partkey, supp_num);
        line.quantity *= 100;
        line.eprice = rprice * line.quantity / 100;

        o.totalprice += ((line.eprice * (100 - line.discount)) / PENNIES) * (100 + line.tax) / PENNIES;

        let s_date = dss_random(L_SDTE_MIN, L_SDTE_MAX, &mut ctx.seeds[stream::L_SDTE_SD]) + tmp_date;
        let c_date = dss_random(L_CDTE_MIN, L_CDTE_MAX, &mut ctx.seeds[stream::L_CDTE_SD]) + tmp_date;
        let r_date = dss_random(L_RDTE_MIN, L_RDTE_MAX, &mut ctx.seeds[stream::L_RDTE_SD]) + s_date;

        line.sdate = (s_date - STARTDATE) as usize;
        line.cdate = (c_date - STARTDATE) as usize;
        line.rdate = (r_date - STARTDATE) as usize;

        line.rflag = if julian(r_date) <= CURRENTDATE {
            dists.l_rflag.pick(&mut ctx.seeds[stream::L_RFLG_SD])[0]
        } else {
            b'N'
        };

        line.lstatus = if julian(s_date) <= CURRENTDATE {
            ocnt += 1;
            b'F'
        } else {
            b'O'
        };
        o.l[lcnt] = line;
    }

    if ocnt > 0 {
        o.orderstatus = b'P';
    }
    if ocnt == o.lines {
        o.orderstatus = b'F';
    }
}

/// `mk_part`: part `index` and its partsupp rows.
pub(crate) fn mk_part(index: i64, p: &mut Part, ctx: &mut DbgenContext) {
    let dists = distributions();
    p.partkey = index;
    agg_str(
        &dists.colors,
        P_NAME_SCL,
        &mut ctx.seeds[stream::P_NAME_SD],
        &mut p.name,
        &mut ctx.permute,
    );
    let temp = dss_random(P_MFG_MIN, P_MFG_MAX, &mut ctx.seeds[stream::P_MFG_SD]);
    push_tagged_number(&mut p.mfgr, b"Manufacturer#", temp, 1);
    let brnd = dss_random(P_BRND_MIN, P_BRND_MAX, &mut ctx.seeds[stream::P_BRND_SD]);
    push_tagged_number(&mut p.brand, b"Brand#", temp * 10 + brnd, 2);
    p.type_ = dists.p_types.pick(&mut ctx.seeds[stream::P_TYPE_SD]);
    p.size = dss_random(P_SIZE_MIN, P_SIZE_MAX, &mut ctx.seeds[stream::P_SIZE_SD]);
    p.container = dists.p_cntr.pick(&mut ctx.seeds[stream::P_CNTR_SD]);
    p.retailprice = rpb_routine(index);
    p.comment = text(P_CMNT_LEN, &mut ctx.seeds[stream::P_CMNT_SD]);

    for snum in 0..SUPP_PER_PART {
        let supplier = &mut p.s[snum];
        supplier.partkey = p.partkey;
        supplier.suppkey = part_supp_bridge(ctx, index, snum as i64);
        supplier.qty = dss_random(PS_QTY_MIN, PS_QTY_MAX, &mut ctx.seeds[stream::PS_QTY_SD]);
        supplier.scost = dss_random(PS_SCST_MIN, PS_SCST_MAX, &mut ctx.seeds[stream::PS_SCST_SD]);
        supplier.comment = text(PS_CMNT_LEN, &mut ctx.seeds[stream::PS_CMNT_SD]);
    }
}

/// `mk_supp`: supplier `index`; one in about a thousand gets a complaint or a
/// recommendation of the Better Business Bureau in its comment.
pub(crate) fn mk_supp(index: i64, s: &mut Supplier, ctx: &mut DbgenContext) {
    let dists = distributions();
    s.suppkey = index;
    push_tagged_number(&mut s.name, b"Supplier#", index, 9);
    v_str(S_ADDR_LEN, &mut ctx.seeds[stream::S_ADDR_SD], &mut s.address);
    let i = dss_random(
        0,
        dists.nations.count() as i64 - 1,
        &mut ctx.seeds[stream::S_NTRG_SD],
    );
    s.nation_code = i;
    s.phone = gen_phone(i, &mut ctx.seeds[stream::S_PHNE_SD]);
    s.acctbal = dss_random(S_ABAL_MIN, S_ABAL_MAX, &mut ctx.seeds[stream::S_ABAL_SD]);

    s.comment.clear();
    s.comment
        .extend_from_slice(text(S_CMNT_LEN, &mut ctx.seeds[stream::S_CMNT_SD]));
    let clen = s.comment.len() as i64;
    // These draws happen for every row, which keeps the streams aligned for parallel
    // generation.
    let bad_press = dss_random(1, 10000, &mut ctx.seeds[stream::BBB_CMNT_SD]);
    let kind = dss_random(0, 100, &mut ctx.seeds[stream::BBB_TYPE_SD]);
    let noise = dss_random(0, clen - BBB_CMNT_LEN, &mut ctx.seeds[stream::BBB_JNK_SD]);
    let offset = dss_random(
        0,
        clen - (BBB_CMNT_LEN + noise),
        &mut ctx.seeds[stream::BBB_OFFSET_SD],
    );
    if bad_press <= S_CMNT_BBB {
        let (offset, noise) = (offset as usize, noise as usize);
        s.comment[offset..offset + BBB_BASE_LEN].copy_from_slice(BBB_BASE);
        let verdict = if kind < BBB_DEADBEATS {
            BBB_COMPLAIN
        } else {
            BBB_COMMEND
        };
        let start = BBB_BASE_LEN + offset + noise;
        s.comment[start..start + verdict.len()].copy_from_slice(verdict);
    }
}

/// `mk_nation`: nation `index` (1-based); its weight in the distribution is its region.
pub(crate) fn mk_nation(index: i64, c: &mut Code, ctx: &mut DbgenContext) {
    let nations = &distributions().nations;
    let (join, text_) = nations.list[index as usize - 1];
    c.code = index - 1;
    c.text = text_;
    c.join = join;
    c.comment = text(N_CMNT_LEN, &mut ctx.seeds[stream::N_CMNT_SD]);
}

/// `mk_region`: region `index` (1-based).
pub(crate) fn mk_region(index: i64, c: &mut Code, ctx: &mut DbgenContext) {
    c.code = index - 1;
    c.text = distributions().regions.member(index as usize - 1);
    c.join = 0;
    c.comment = text(R_CMNT_LEN, &mut ctx.seeds[stream::R_CMNT_SD]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_numbers_are_zero_padded() {
        let mut dest = Vec::new();
        push_tagged_number(&mut dest, b"Customer#", 42, 9);
        assert_eq!(dest, b"Customer#000000042");
        push_tagged_number(&mut dest, b"Brand#", 123, 2);
        assert_eq!(dest, b"Brand#123");
        push_tagged_number(&mut dest, b"Manufacturer#", 5, 1);
        assert_eq!(dest, b"Manufacturer#5");
    }

    #[test]
    fn sparse_keys_skip_every_other_block_of_eight() {
        assert_eq!(mk_sparse(1, 0), 1);
        assert_eq!(mk_sparse(7, 0), 7);
        assert_eq!(mk_sparse(8, 0), 32);
        assert_eq!(mk_sparse(9, 0), 33);
    }
}
