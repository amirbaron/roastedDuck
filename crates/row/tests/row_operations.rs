//! Row matching and aggregate states in rows.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::{Rng, TestValue, buffer_manager, build_chunks, random_value, struct_type};
use rustdb_buffer::MemoryTag;
use rustdb_common::Result;
use rustdb_function::AggregateFunction;
use rustdb_function::aggregate::{Unary, UnaryAggregate};
use rustdb_row::{
    ChunkFormat, MatchPredicate, PinProperties, RowAggregate, RowMatcher, RowOperationsState, RowPtr,
    TupleDataCollection, TupleDataLayout, ValidityType, combine_states, destroy_states, finalize_states,
    initialize_states, update_states,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

fn match_types() -> Vec<LogicalType> {
    vec![
        LogicalType::INTEGER,
        LogicalType::VARCHAR,
        struct_type(&[("x", LogicalType::INTEGER), ("label", LogicalType::VARCHAR)]),
        LogicalType::list(LogicalType::INTEGER),
        LogicalType::INTERVAL,
    ]
}

/// Stored rows plus probe rows that equal their stored row about half of the time.
fn match_data(rng: &mut Rng, count: usize) -> (Vec<Vec<TestValue>>, Vec<Vec<TestValue>>) {
    let types = match_types();
    let stored: Vec<Vec<TestValue>> = (0..count)
        .map(|_| types.iter().map(|ty| random_value(ty, rng, 10)).collect())
        .collect();
    let probes = stored
        .iter()
        .map(|row| {
            row.iter()
                .zip(&types)
                .map(|(value, ty)| {
                    if rng.chance(70) {
                        value.clone()
                    } else {
                        random_value(ty, rng, 10)
                    }
                })
                .collect()
        })
        .collect();
    (stored, probes)
}

fn expected_match(predicate: MatchPredicate, lhs: &TestValue, rhs: &TestValue) -> bool {
    let null = |value: &TestValue| matches!(value, TestValue::Null);
    match predicate {
        MatchPredicate::Equal => !null(lhs) && !null(rhs) && lhs == rhs,
        MatchPredicate::NotEqual => !null(lhs) && !null(rhs) && lhs != rhs,
        MatchPredicate::DistinctFrom => lhs != rhs,
        MatchPredicate::NotDistinctFrom => lhs == rhs,
        _ => unreachable!("only equality predicates are checked against value equality"),
    }
}

#[test]
fn matcher_agrees_with_value_semantics() {
    let types = match_types();
    let mut rng = Rng::new(99);
    let (stored, probes) = match_data(&mut rng, 2000);
    let layout = Arc::new(TupleDataLayout::new(types.clone(), ValidityType::CanHaveNull).unwrap());
    let mut rows =
        TupleDataCollection::new(buffer_manager(), Arc::clone(&layout), MemoryTag::HashTable).unwrap();
    let mut append = rows.initialize_append(PinProperties::KeepEverythingPinned);
    let stored_chunk = &build_chunks(&types, &stored, 2000)[0];
    rows.append(&mut append, stored_chunk).unwrap();
    let locations: Vec<RowPtr> = append.chunk_state.row_locations()[..2000].to_vec();
    rows.finalize_pin_state(&mut append.pin_state);
    let probe_chunk = &build_chunks(&types, &probes, 2000)[0];
    let formats = ChunkFormat::new(probe_chunk).unwrap();

    for predicate in [
        MatchPredicate::Equal,
        MatchPredicate::NotEqual,
        MatchPredicate::DistinctFrom,
        MatchPredicate::NotDistinctFrom,
    ] {
        for column in 0..types.len() {
            let matcher = RowMatcher::new(&layout, &[predicate], Some(vec![column])).unwrap();
            let mut sel: Vec<u32> = (0..2000).collect();
            let mut no_match = Vec::new();
            // SAFETY: the stored rows stay pinned while `rows` lives.
            let matched = unsafe {
                matcher
                    .match_rows(&layout, &formats, &mut sel, 2000, &locations, Some(&mut no_match))
                    .unwrap()
            };
            let expected: Vec<u32> = (0..2000u32)
                .filter(|&i| {
                    expected_match(
                        predicate,
                        &probes[i as usize][column],
                        &stored[i as usize][column],
                    )
                })
                .collect();
            assert_eq!(
                &sel[..matched],
                expected.as_slice(),
                "{predicate:?} on {}",
                types[column]
            );
            assert_eq!(matched + no_match.len(), 2000);
        }
    }

    // All columns at once: a row matches only if every column does.
    let predicates = vec![MatchPredicate::NotDistinctFrom; types.len()];
    let matcher = RowMatcher::new(&layout, &predicates, None).unwrap();
    let mut sel: Vec<u32> = (0..2000).collect();
    // SAFETY: as above.
    let matched = unsafe {
        matcher
            .match_rows(&layout, &formats, &mut sel, 2000, &locations, None)
            .unwrap()
    };
    let expected: Vec<u32> = (0..2000u32)
        .filter(|&i| probes[i as usize] == stored[i as usize])
        .collect();
    assert_eq!(&sel[..matched], expected.as_slice());
}

#[test]
fn ordering_predicates_on_integers_and_strings() {
    let types = vec![
        LogicalType::INTEGER,
        LogicalType::VARCHAR,
        LogicalType::list(LogicalType::INTEGER),
    ];
    let layout = Arc::new(TupleDataLayout::new(types.clone(), ValidityType::CanHaveNull).unwrap());
    let mut rows =
        TupleDataCollection::new(buffer_manager(), Arc::clone(&layout), MemoryTag::HashTable).unwrap();
    let int = |v: i32| TestValue::Fixed(v.to_ne_bytes().to_vec());
    let string = |s: &str| TestValue::Str(s.as_bytes().to_vec());
    let list = |v: &[i32]| TestValue::List(v.iter().map(|&v| int(v)).collect());
    let stored = vec![
        vec![int(5), string("banana but longer than inline"), list(&[1, 2])],
        vec![int(5), string("b"), list(&[1, 2, 3])],
        vec![TestValue::Null, TestValue::Null, TestValue::Null],
        vec![int(-3), string("zzz"), list(&[])],
    ];
    let probes = vec![
        vec![int(7), string("banana but longer than inline!"), list(&[1, 3])],
        vec![int(5), string("a"), list(&[1, 2])],
        vec![int(1), string("x"), list(&[0])],
        vec![int(-4), string("zzz"), TestValue::Null],
    ];
    let mut append = rows.initialize_append(PinProperties::KeepEverythingPinned);
    rows.append(&mut append, &build_chunks(&types, &stored, 4)[0])
        .unwrap();
    let locations = append.chunk_state.row_locations()[..4].to_vec();
    rows.finalize_pin_state(&mut append.pin_state);
    let probe_chunk = &build_chunks(&types, &probes, 4)[0];
    let formats = ChunkFormat::new(probe_chunk).unwrap();
    let run = |predicate: MatchPredicate, column: usize| -> Vec<u32> {
        let matcher = RowMatcher::new(&layout, &[predicate], Some(vec![column])).unwrap();
        let mut sel: Vec<u32> = (0..4).collect();
        // SAFETY: rows pinned while `rows` lives.
        let matched = unsafe {
            matcher
                .match_rows(&layout, &formats, &mut sel, 4, &locations, None)
                .unwrap()
        };
        sel[..matched].to_vec()
    };
    assert_eq!(run(MatchPredicate::GreaterThan, 0), vec![0]);
    assert_eq!(run(MatchPredicate::GreaterThanOrEqual, 0), vec![0, 1]);
    assert_eq!(run(MatchPredicate::LessThan, 0), vec![3]);
    assert_eq!(run(MatchPredicate::LessThanOrEqual, 0), vec![1, 3]);
    assert_eq!(run(MatchPredicate::GreaterThan, 1), vec![0]);
    assert_eq!(run(MatchPredicate::LessThanOrEqual, 1), vec![1, 3]);
    // Nested comparisons put NULL last: [0] < NULL, NULL > [].
    assert_eq!(run(MatchPredicate::GreaterThan, 2), vec![0, 3]);
    assert_eq!(run(MatchPredicate::LessThan, 2), vec![1, 2]);
}

/// `sum(BIGINT)` keeping its inputs in a `Vec` (a state with a destructor).
struct CollectSum;

impl UnaryAggregate for CollectSum {
    type Input = i64;
    type State = Vec<i64>;
    type Output = i64;

    fn update(state: &mut Vec<i64>, value: i64) -> Result<()> {
        state.push(value);
        Ok(())
    }

    fn combine(source: &Vec<i64>, target: &mut Vec<i64>) -> Result<()> {
        target.extend_from_slice(source);
        Ok(())
    }

    fn finalize(state: &Vec<i64>) -> Result<Option<i64>> {
        Ok((!state.is_empty()).then(|| state.iter().sum()))
    }
}

#[test]
fn aggregate_states_live_in_rows() {
    let function = AggregateFunction::new(
        "collect_sum",
        vec![LogicalType::BIGINT],
        LogicalType::BIGINT,
        Unary::<CollectSum>::default(),
    );
    let aggregate = RowAggregate::new(function, None, 1);
    assert_eq!(aggregate.payload_size, 24);
    let layout = Arc::new(
        TupleDataLayout::with_aggregates(
            vec![LogicalType::INTEGER],
            vec![aggregate],
            ValidityType::CanHaveNull,
        )
        .unwrap(),
    );
    assert!(layout.has_destructor());
    assert_eq!(layout.aggr_offset() % 8, 0);
    let mut groups =
        TupleDataCollection::new(buffer_manager(), Arc::clone(&layout), MemoryTag::HashTable).unwrap();
    let mut append = groups.initialize_append_columns(vec![0], PinProperties::KeepEverythingPinned);
    let mut keys = Vector::flat(LogicalType::INTEGER, 4);
    keys.values_mut::<i32>().copy_from_slice(&[1, 2, 3, 4]);
    let key_chunk = DataChunk::new(vec![keys], 4);
    // Group rows have no aggregate input column; the layout only appends the key.
    groups.append(&mut append, &key_chunk).unwrap();
    let rows = append.chunk_state.row_locations()[..4].to_vec();
    groups.finalize_pin_state(&mut append.pin_state);
    let mut state = RowOperationsState::default();
    // SAFETY: the rows stay pinned and their states are used single-threaded.
    unsafe {
        initialize_states(&layout, &rows, None, 4).unwrap();
        // Two inputs per group: rows i and (i + 1) % 4.
        let mut inputs = Vector::flat(LogicalType::BIGINT, 4);
        inputs.values_mut::<i64>().copy_from_slice(&[10, 20, 30, 40]);
        update_states(&mut state, &layout, 0, &rows, std::slice::from_ref(&inputs), 4).unwrap();
        let shifted = [rows[1], rows[2], rows[3], rows[0]];
        update_states(&mut state, &layout, 0, &shifted, std::slice::from_ref(&inputs), 4).unwrap();
        // Combine group 0 into group 1.
        combine_states(&mut state, &layout, &rows[..1], &rows[1..2], 1).unwrap();
        let mut results = vec![Vector::flat(LogicalType::BIGINT, 4)];
        finalize_states(&mut state, &layout, &rows, 4, &mut results, 0).unwrap();
        assert_eq!(
            (0..4).map(|row| results[0].get_value(row)).collect::<Vec<_>>(),
            vec![
                Value::bigint(50),
                Value::bigint(80),
                Value::bigint(50),
                Value::bigint(70)
            ]
        );
        destroy_states(&mut state, &layout, &rows, 4).unwrap();
    }
}
