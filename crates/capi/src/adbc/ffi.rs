// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

// Rust ABI transcription of the frozen DuckDB 1.5.6 ADBC header.
// ADBC C interface declarations derived from the frozen Apache Arrow ADBC header.
// Licensed under the Apache License, Version 2.0; see the upstream header and NOTICE.
#![allow(non_snake_case)]
use rustdb_arrow::ffi::{ArrowArray, ArrowArrayStream, ArrowSchema};
use std::ffi::{c_char, c_int, c_void};
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcError {
    pub message: *mut c_char,
    pub vendor_code: i32,
    pub sqlstate: [c_char; 5],
    pub release: Option<unsafe extern "C" fn(*mut AdbcError)>,
    pub private_data: *mut c_void,
    pub private_driver: *mut AdbcDriver,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcErrorDetail {
    pub key: *const c_char,
    pub value: *const u8,
    pub value_length: usize,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcDatabase {
    pub private_data: *mut c_void,
    pub private_driver: *mut AdbcDriver,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcConnection {
    pub private_data: *mut c_void,
    pub private_driver: *mut AdbcDriver,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcStatement {
    pub private_data: *mut c_void,
    pub private_driver: *mut AdbcDriver,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcPartitions {
    pub num_partitions: usize,
    pub partitions: *mut *const u8,
    pub partition_lengths: *const usize,
    pub private_data: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut AdbcPartitions)>,
}
#[repr(C)]
#[derive(Debug, Default)]
pub struct AdbcDriver {
    pub private_data: *mut c_void,
    pub private_manager: *mut c_void,
    pub release: Option<unsafe extern "C" fn(*mut AdbcDriver, *mut AdbcError) -> u8>,
    pub DatabaseInit: Option<unsafe extern "C" fn(*mut AdbcDatabase, *mut AdbcError) -> u8>,
    pub DatabaseNew: Option<unsafe extern "C" fn(*mut AdbcDatabase, *mut AdbcError) -> u8>,
    pub DatabaseSetOption:
        Option<unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *const c_char, *mut AdbcError) -> u8>,
    pub DatabaseRelease: Option<unsafe extern "C" fn(*mut AdbcDatabase, *mut AdbcError) -> u8>,
    pub ConnectionCommit: Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcError) -> u8>,
    pub ConnectionGetInfo: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            *const u32,
            usize,
            *mut ArrowArrayStream,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionGetObjects: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            c_int,
            *const c_char,
            *const c_char,
            *const c_char,
            *mut *const c_char,
            *const c_char,
            *mut ArrowArrayStream,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionGetTableSchema: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            *const c_char,
            *const c_char,
            *const c_char,
            *mut ArrowSchema,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionGetTableTypes:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *mut ArrowArrayStream, *mut AdbcError) -> u8>,
    pub ConnectionInit:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcDatabase, *mut AdbcError) -> u8>,
    pub ConnectionNew: Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcError) -> u8>,
    pub ConnectionSetOption:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *const c_char, *const c_char, *mut AdbcError) -> u8>,
    pub ConnectionReadPartition: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            *const u8,
            usize,
            *mut ArrowArrayStream,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionRelease: Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcError) -> u8>,
    pub ConnectionRollback: Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcError) -> u8>,
    pub StatementBind: Option<
        unsafe extern "C" fn(*mut AdbcStatement, *mut ArrowArray, *mut ArrowSchema, *mut AdbcError) -> u8,
    >,
    pub StatementBindStream:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *mut ArrowArrayStream, *mut AdbcError) -> u8>,
    pub StatementExecuteQuery: Option<
        unsafe extern "C" fn(*mut AdbcStatement, *mut ArrowArrayStream, *mut i64, *mut AdbcError) -> u8,
    >,
    pub StatementExecutePartitions: Option<
        unsafe extern "C" fn(
            *mut AdbcStatement,
            *mut ArrowSchema,
            *mut AdbcPartitions,
            *mut i64,
            *mut AdbcError,
        ) -> u8,
    >,
    pub StatementGetParameterSchema:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *mut ArrowSchema, *mut AdbcError) -> u8>,
    pub StatementNew:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcStatement, *mut AdbcError) -> u8>,
    pub StatementPrepare: Option<unsafe extern "C" fn(*mut AdbcStatement, *mut AdbcError) -> u8>,
    pub StatementRelease: Option<unsafe extern "C" fn(*mut AdbcStatement, *mut AdbcError) -> u8>,
    pub StatementSetOption:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *const c_char, *mut AdbcError) -> u8>,
    pub StatementSetSqlQuery:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *mut AdbcError) -> u8>,
    pub StatementSetSubstraitPlan:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const u8, usize, *mut AdbcError) -> u8>,
    pub ErrorGetDetailCount: Option<unsafe extern "C" fn(*const AdbcError) -> c_int>,
    pub ErrorGetDetail: Option<unsafe extern "C" fn(*const AdbcError, c_int) -> AdbcErrorDetail>,
    pub ErrorFromArrayStream:
        Option<unsafe extern "C" fn(*mut ArrowArrayStream, *mut u8) -> *const AdbcError>,
    pub DatabaseGetOption: Option<
        unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *mut c_char, *mut usize, *mut AdbcError) -> u8,
    >,
    pub DatabaseGetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *mut u8, *mut usize, *mut AdbcError) -> u8,
    >,
    pub DatabaseGetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *mut f64, *mut AdbcError) -> u8>,
    pub DatabaseGetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *mut i64, *mut AdbcError) -> u8>,
    pub DatabaseSetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, *const u8, usize, *mut AdbcError) -> u8,
    >,
    pub DatabaseSetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, f64, *mut AdbcError) -> u8>,
    pub DatabaseSetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcDatabase, *const c_char, i64, *mut AdbcError) -> u8>,
    pub ConnectionCancel: Option<unsafe extern "C" fn(*mut AdbcConnection, *mut AdbcError) -> u8>,
    pub ConnectionGetOption: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            *const c_char,
            *mut c_char,
            *mut usize,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionGetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcConnection, *const c_char, *mut u8, *mut usize, *mut AdbcError) -> u8,
    >,
    pub ConnectionGetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *const c_char, *mut f64, *mut AdbcError) -> u8>,
    pub ConnectionGetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *const c_char, *mut i64, *mut AdbcError) -> u8>,
    pub ConnectionGetStatistics: Option<
        unsafe extern "C" fn(
            *mut AdbcConnection,
            *const c_char,
            *const c_char,
            *const c_char,
            c_char,
            *mut ArrowArrayStream,
            *mut AdbcError,
        ) -> u8,
    >,
    pub ConnectionGetStatisticNames:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *mut ArrowArrayStream, *mut AdbcError) -> u8>,
    pub ConnectionSetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcConnection, *const c_char, *const u8, usize, *mut AdbcError) -> u8,
    >,
    pub ConnectionSetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *const c_char, f64, *mut AdbcError) -> u8>,
    pub ConnectionSetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcConnection, *const c_char, i64, *mut AdbcError) -> u8>,
    pub StatementCancel: Option<unsafe extern "C" fn(*mut AdbcStatement, *mut AdbcError) -> u8>,
    pub StatementExecuteSchema:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *mut ArrowSchema, *mut AdbcError) -> u8>,
    pub StatementGetOption: Option<
        unsafe extern "C" fn(
            *mut AdbcStatement,
            *const c_char,
            *mut c_char,
            *mut usize,
            *mut AdbcError,
        ) -> u8,
    >,
    pub StatementGetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *mut u8, *mut usize, *mut AdbcError) -> u8,
    >,
    pub StatementGetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *mut f64, *mut AdbcError) -> u8>,
    pub StatementGetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *mut i64, *mut AdbcError) -> u8>,
    pub StatementSetOptionBytes: Option<
        unsafe extern "C" fn(*mut AdbcStatement, *const c_char, *const u8, usize, *mut AdbcError) -> u8,
    >,
    pub StatementSetOptionDouble:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, f64, *mut AdbcError) -> u8>,
    pub StatementSetOptionInt:
        Option<unsafe extern "C" fn(*mut AdbcStatement, *const c_char, i64, *mut AdbcError) -> u8>,
}

// Frozen DuckDB 1.5.6 header layout, independently measured by the signed C++ probe.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<AdbcError>() == 48);
    assert!(size_of::<AdbcDatabase>() == 16);
    assert!(size_of::<AdbcConnection>() == 16);
    assert!(size_of::<AdbcStatement>() == 16);
    assert!(size_of::<AdbcPartitions>() == 40);
    assert!(size_of::<AdbcErrorDetail>() == 24);
    assert!(size_of::<AdbcDriver>() == 464);
    assert!(offset_of!(AdbcError, release) == 24);
    assert!(offset_of!(AdbcError, private_data) == 32);
    assert!(offset_of!(AdbcDriver, release) == 16);
    assert!(offset_of!(AdbcDriver, StatementSetSubstraitPlan) == 224);
    assert!(offset_of!(AdbcDriver, ErrorGetDetailCount) == 232);
    assert!(offset_of!(AdbcDriver, StatementSetOptionInt) == 456);
};
