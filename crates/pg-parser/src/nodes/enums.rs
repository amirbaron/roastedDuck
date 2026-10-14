//! Enumerations used by parse-tree nodes, mirroring `libpg_query`'s C enums.
//!
//! Discriminants equal the C values, so `as i32` gives the number DuckDB's
//! transformer sees.

/// `PGAExpr_Kind`: `PGAExpr` - infix, prefix, and postfix expressions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum AExprKind {
    /// Normal operator (`PG_AEXPR_OP`)
    #[default]
    Op = 0,
    /// Scalar op ANY (array) (`PG_AEXPR_OP_ANY`)
    OpAny = 1,
    /// Scalar op ALL (array) (`PG_AEXPR_OP_ALL`)
    OpAll = 2,
    /// IS DISTINCT FROM - name must be "=" (`PG_AEXPR_DISTINCT`)
    Distinct = 3,
    /// IS NOT DISTINCT FROM - name must be "=" (`PG_AEXPR_NOT_DISTINCT`)
    NotDistinct = 4,
    /// NULLIF - name must be "=" (`PG_AEXPR_NULLIF`)
    Nullif = 5,
    /// IS [NOT] OF - name must be "=" or "<>" (`PG_AEXPR_OF`)
    Of = 6,
    /// [NOT] IN - name must be "=" or "<>" (`PG_AEXPR_IN`)
    In = 7,
    /// [NOT] LIKE - name must be "~~" or "!~~" (`PG_AEXPR_LIKE`)
    Like = 8,
    /// [NOT] ILIKE - name must be "~~*" or "!~~*" (`PG_AEXPR_ILIKE`)
    Ilike = 9,
    /// [NOT] GLOB - name must be "~~~" or "!~~~" (`PG_AEXPR_GLOB`)
    Glob = 10,
    /// [NOT] SIMILAR - name must be "~" or "!~" (`PG_AEXPR_SIMILAR`)
    Similar = 11,
    /// Name must be "BETWEEN" (`PG_AEXPR_BETWEEN`)
    Between = 12,
    /// Name must be "NOT BETWEEN" (`PG_AEXPR_NOT_BETWEEN`)
    NotBetween = 13,
    /// Name must be "BETWEEN SYMMETRIC" (`PG_AEXPR_BETWEEN_SYM`)
    BetweenSym = 14,
    /// Name must be "NOT BETWEEN SYMMETRIC" (`PG_AEXPR_NOT_BETWEEN_SYM`)
    NotBetweenSym = 15,
    /// `AEXPR_PAREN`.
    Paren = 16,
}

/// `PGAlterDatabaseType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum AlterDatabaseType {
    /// `PG_ALTER_DATABASE_RENAME`.
    #[default]
    Rename = 0,
}

/// `PGAlterTableType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum AlterTableType {
    /// Add column (`PG_AT_AddColumn`)
    #[default]
    AddColumn = 0,
    /// Internal to commands/`tablecmds.c` (`PG_AT_AddColumnRecurse`)
    AddColumnRecurse = 1,
    /// Implicitly via CREATE OR REPLACE VIEW (`PG_AT_AddColumnToView`)
    AddColumnToView = 2,
    /// Alter column default (`PG_AT_ColumnDefault`)
    ColumnDefault = 3,
    /// Alter column drop not null (`PG_AT_DropNotNull`)
    DropNotNull = 4,
    /// Alter column set not null (`PG_AT_SetNotNull`)
    SetNotNull = 5,
    /// Alter column set statistics (`PG_AT_SetStatistics`)
    SetStatistics = 6,
    /// Alter column set ( options ) (`PG_AT_SetOptions`)
    SetOptions = 7,
    /// Alter column reset ( options ) (`PG_AT_ResetOptions`)
    ResetOptions = 8,
    /// Alter column set storage (`PG_AT_SetStorage`)
    SetStorage = 9,
    /// Drop column (`PG_AT_DropColumn`)
    DropColumn = 10,
    /// Internal to commands/`tablecmds.c` (`PG_AT_DropColumnRecurse`)
    DropColumnRecurse = 11,
    /// Add index (`PG_AT_AddIndex`)
    AddIndex = 12,
    /// Internal to commands/`tablecmds.c` (`PG_AT_ReAddIndex`)
    ReAddIndex = 13,
    /// Add constraint (`PG_AT_AddConstraint`)
    AddConstraint = 14,
    /// Internal to commands/`tablecmds.c` (`PG_AT_AddConstraintRecurse`)
    AddConstraintRecurse = 15,
    /// Internal to commands/`tablecmds.c` (`PG_AT_ReAddConstraint`)
    ReAddConstraint = 16,
    /// Alter constraint (`PG_AT_AlterConstraint`)
    AlterConstraint = 17,
    /// Validate constraint (`PG_AT_ValidateConstraint`)
    ValidateConstraint = 18,
    /// Internal to commands/`tablecmds.c` (`PG_AT_ValidateConstraintRecurse`)
    ValidateConstraintRecurse = 19,
    /// Pre-processed add constraint (local in parser/`parse_utilcmd.c`)
    /// (`PG_AT_ProcessedConstraint`)
    ProcessedConstraint = 20,
    /// Add constraint using existing index (`PG_AT_AddIndexConstraint`)
    AddIndexConstraint = 21,
    /// Drop constraint (`PG_AT_DropConstraint`)
    DropConstraint = 22,
    /// Internal to commands/`tablecmds.c` (`PG_AT_DropConstraintRecurse`)
    DropConstraintRecurse = 23,
    /// Internal to commands/`tablecmds.c` (`PG_AT_ReAddComment`)
    ReAddComment = 24,
    /// Alter column type (`PG_AT_AlterColumnType`)
    AlterColumnType = 25,
    /// Alter column OPTIONS (...) (`PG_AT_AlterColumnGenericOptions`)
    AlterColumnGenericOptions = 26,
    /// Change owner (`PG_AT_ChangeOwner`)
    ChangeOwner = 27,
    /// CLUSTER ON (`PG_AT_ClusterOn`)
    ClusterOn = 28,
    /// SET WITHOUT CLUSTER (`PG_AT_DropCluster`)
    DropCluster = 29,
    /// SET LOGGED (`PG_AT_SetLogged`)
    SetLogged = 30,
    /// SET UNLOGGED (`PG_AT_SetUnLogged`)
    SetUnLogged = 31,
    /// SET WITH OIDS (`PG_AT_AddOids`)
    AddOids = 32,
    /// Internal to commands/`tablecmds.c` (`PG_AT_AddOidsRecurse`)
    AddOidsRecurse = 33,
    /// SET WITHOUT OIDS (`PG_AT_DropOids`)
    DropOids = 34,
    /// SET TABLESPACE (`PG_AT_SetTableSpace`)
    SetTableSpace = 35,
    /// SET (...) -- AM specific parameters (`PG_AT_SetRelOptions`)
    SetRelOptions = 36,
    /// RESET (...) -- AM specific parameters (`PG_AT_ResetRelOptions`)
    ResetRelOptions = 37,
    /// Replace reloption list in its entirety (`PG_AT_ReplaceRelOptions`)
    ReplaceRelOptions = 38,
    /// ENABLE TRIGGER name (`PG_AT_EnableTrig`)
    EnableTrig = 39,
    /// ENABLE ALWAYS TRIGGER name (`PG_AT_EnableAlwaysTrig`)
    EnableAlwaysTrig = 40,
    /// ENABLE REPLICA TRIGGER name (`PG_AT_EnableReplicaTrig`)
    EnableReplicaTrig = 41,
    /// DISABLE TRIGGER name (`PG_AT_DisableTrig`)
    DisableTrig = 42,
    /// ENABLE TRIGGER ALL (`PG_AT_EnableTrigAll`)
    EnableTrigAll = 43,
    /// DISABLE TRIGGER ALL (`PG_AT_DisableTrigAll`)
    DisableTrigAll = 44,
    /// ENABLE TRIGGER USER (`PG_AT_EnableTrigUser`)
    EnableTrigUser = 45,
    /// DISABLE TRIGGER USER (`PG_AT_DisableTrigUser`)
    DisableTrigUser = 46,
    /// ENABLE RULE name (`PG_AT_EnableRule`)
    EnableRule = 47,
    /// ENABLE ALWAYS RULE name (`PG_AT_EnableAlwaysRule`)
    EnableAlwaysRule = 48,
    /// ENABLE REPLICA RULE name (`PG_AT_EnableReplicaRule`)
    EnableReplicaRule = 49,
    /// DISABLE RULE name (`PG_AT_DisableRule`)
    DisableRule = 50,
    /// INHERIT parent (`PG_AT_AddInherit`)
    AddInherit = 51,
    /// NO INHERIT parent (`PG_AT_DropInherit`)
    DropInherit = 52,
    /// OF <`type_name`> (`PG_AT_AddOf`)
    AddOf = 53,
    /// NOT OF (`PG_AT_DropOf`)
    DropOf = 54,
    /// REPLICA IDENTITY (`PG_AT_ReplicaIdentity`)
    ReplicaIdentity = 55,
    /// ENABLE ROW SECURITY (`PG_AT_EnableRowSecurity`)
    EnableRowSecurity = 56,
    /// DISABLE ROW SECURITY (`PG_AT_DisableRowSecurity`)
    DisableRowSecurity = 57,
    /// FORCE ROW SECURITY (`PG_AT_ForceRowSecurity`)
    ForceRowSecurity = 58,
    /// NO FORCE ROW SECURITY (`PG_AT_NoForceRowSecurity`)
    NoForceRowSecurity = 59,
    /// OPTIONS (...) (`PG_AT_GenericOptions`)
    GenericOptions = 60,
    /// ATTACH PARTITION (`PG_AT_AttachPartition`)
    AttachPartition = 61,
    /// DETACH PARTITION (`PG_AT_DetachPartition`)
    DetachPartition = 62,
    /// ADD IDENTITY (`PG_AT_AddIdentity`)
    AddIdentity = 63,
    /// SET identity column options (`PG_AT_SetIdentity`)
    SetIdentity = 64,
    /// DROP IDENTITY (`AT_DropIdentity`)
    DropIdentity = 65,
    /// SET PARTITIONED BY (`PG_AT_SetPartitionedBy`)
    SetPartitionedBy = 66,
    /// SET SORTED BY (`PG_AT_SetSortedBy`)
    SetSortedBy = 67,
}

/// `PGBoolExprType`: `PGBoolExpr` - expression node for the basic Boolean operators AND, OR, NOT
/// Notice the arguments are given as a List.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum BoolExprType {
    /// `PG_AND_EXPR`.
    #[default]
    And = 0,
    /// `PG_OR_EXPR`.
    Or = 1,
    /// `PG_NOT_EXPR`.
    Not = 2,
}

/// `PGBoolTestType`: `PGBooleanTest` `PGBooleanTest` represents the operation of determining
/// whether a boolean is true, false, or UNKNOWN (ie, NULL)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum BoolTestType {
    /// `PG_IS_TRUE`.
    #[default]
    True = 0,
    /// `IS_NOT_TRUE`.
    NotTrue = 1,
    /// `IS_FALSE`.
    False = 2,
    /// `IS_NOT_FALSE`.
    NotFalse = 3,
    /// `IS_UNKNOWN`.
    Unknown = 4,
    /// `IS_NOT_UNKNOWN`.
    NotUnknown = 5,
}

/// `PGCTEMaterialize`: `PGCommonTableExpr` - representation of WITH list element We don't currently
/// support the SEARCH or CYCLE clause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum CteMaterialize {
    /// No option specified (`PGCTEMaterializeDefault`)
    #[default]
    Default = 0,
    /// MATERIALIZED (`PGCTEMaterializeAlways`)
    Always = 1,
    /// `PGCTEMaterializeNever`.
    Never = 2,
}

/// `ColumnCategory`: Category of the column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ColumnCategory {
    /// Regular column (`COL_STANDARD`)
    #[default]
    Standard = 0,
    /// `COL_GENERATED`.
    Generated = 1,
}

/// `PGConstrType`: Definitions for constraints in `PGCreateStmt` Note that column defaults are
/// treated as a type of constraint, even though that's a bit odd semantically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ConstrType {
    /// Not standard SQL, but a lot of people expect it (`PG_CONSTR_NULL`)
    #[default]
    Null = 0,
    /// `PG_CONSTR_NOTNULL`.
    NotNull = 1,
    /// `PG_CONSTR_DEFAULT`.
    Default = 2,
    /// `PG_CONSTR_IDENTITY`.
    Identity = 3,
    /// `PG_CONSTR_CHECK`.
    Check = 4,
    /// `PG_CONSTR_PRIMARY`.
    Primary = 5,
    /// `PG_CONSTR_UNIQUE`.
    Unique = 6,
    /// `PG_CONSTR_EXCLUSION`.
    Exclusion = 7,
    /// `PG_CONSTR_FOREIGN`.
    Foreign = 8,
    /// Attributes for previous constraint node (`PG_CONSTR_ATTR_DEFERRABLE`)
    AttrDeferrable = 9,
    /// `PG_CONSTR_ATTR_NOT_DEFERRABLE`.
    AttrNotDeferrable = 10,
    /// `PG_CONSTR_ATTR_DEFERRED`.
    AttrDeferred = 11,
    /// `PG_CONSTR_ATTR_IMMEDIATE`.
    AttrImmediate = 12,
    /// `PG_CONSTR_COMPRESSION`.
    Compression = 13,
    /// `PG_CONSTR_GENERATED_VIRTUAL`.
    GeneratedVirtual = 14,
    /// `PG_CONSTR_GENERATED_STORED`.
    GeneratedStored = 15,
}

/// `PGDefElemAction`: `PGDefElem` - a generic "name = value" option definition In some contexts the
/// name can be qualified.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum DefElemAction {
    /// No action given (`PG_DEFELEM_UNSPEC`)
    #[default]
    Unspec = 0,
    /// `PG_DEFELEM_SET`.
    Set = 1,
    /// `PG_DEFELEM_ADD`.
    Add = 2,
    /// `DEFELEM_DROP`.
    Drop = 3,
}

/// `PGDropBehavior`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum DropBehavior {
    /// Drop fails if any dependent objects (`PG_DROP_RESTRICT`)
    #[default]
    Restrict = 0,
    /// `PG_DROP_CASCADE`.
    Cascade = 1,
}

/// `GroupingSetKind`: `PGGroupingSet` - representation of CUBE, ROLLUP and GROUPING SETS clauses In
/// a `PGQuery` with grouping sets, the `groupClause` contains a flat list of `PGSortGroupClause`
/// nodes for each distinct expression used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum GroupingSetKind {
    /// `GROUPING_SET_EMPTY`.
    #[default]
    Empty = 0,
    /// `GROUPING_SET_SIMPLE`.
    Simple = 1,
    /// `GROUPING_SET_ROLLUP`.
    Rollup = 2,
    /// `GROUPING_SET_CUBE`.
    Cube = 3,
    /// `GROUPING_SET_SETS`.
    Sets = 4,
    /// `GROUPING_SET_ALL`.
    All = 5,
}

/// `PGIgnoreNulls`: `PGFuncCall` options RESPECT/IGNORE NULLS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum IgnoreNulls {
    /// `PG_DEFAULT_NULLS`.
    #[default]
    Default = 0,
    /// `PG_RESPECT_NULLS`.
    Respect = 1,
    /// `PG_IGNORE_NULLS`.
    Ignore = 2,
}

/// `PGInsertColumnOrder`: `PGInsertByNameOrPosition` "INSERT BY [POSITION|NAME].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum InsertColumnOrder {
    /// INSERT BY POSITION (default behavior) (`PG_INSERT_BY_POSITION`)
    #[default]
    ByPosition = 0,
    /// INSERT BY NAME (`PG_INSERT_BY_NAME`)
    ByName = 1,
}

/// `PGJoinRefType`: enums for the types of implied conditions `PGJoinRefType` specifies the
/// semantics of interpreting the join conditions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum JoinRefType {
    /// Join conditions are interpreted as is (`PG_JOIN_REGULAR`)
    #[default]
    Regular = 0,
    /// Join conditions are inferred from the column names (`PG_JOIN_NATURAL`)
    Natural = 1,
    /// `PG_JOIN_ASOF`.
    Asof = 2,
}

/// `PGJoinType`: enums for types of relation joins `PGJoinType` determines the exact semantics of
/// joining two relations using a matching qualification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum JoinType {
    /// Matching tuple pairs only (`PG_JOIN_INNER`)
    #[default]
    Inner = 0,
    /// Pairs + unmatched LHS tuples (`PG_JOIN_LEFT`)
    Left = 1,
    /// Pairs + unmatched LHS + unmatched RHS (`PG_JOIN_FULL`)
    Full = 2,
    /// Pairs + unmatched RHS tuples (`PG_JOIN_RIGHT`)
    Right = 3,
    /// 1 copy of each LHS row that has match(es) (`PG_JOIN_SEMI`)
    Semi = 4,
    /// 1 copy of each LHS row that has no match (`PG_JOIN_ANTI`)
    Anti = 5,
    /// LHS path must be made unique (`PG_JOIN_UNIQUE_OUTER`)
    UniqueOuter = 6,
    /// RHS path must be made unique (`PG_JOIN_UNIQUE_INNER`)
    UniqueInner = 7,
    /// `PG_JOIN_POSITION`.
    Position = 8,
}

/// `PGLoadInstallType`: Load Statement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum LoadInstallType {
    /// `PG_LOAD_TYPE_LOAD`.
    #[default]
    Load = 0,
    /// `PG_LOAD_TYPE_INSTALL`.
    Install = 1,
    /// `PG_LOAD_TYPE_FORCE_INSTALL`.
    ForceInstall = 2,
}

/// `PGLockClauseStrength`: This enum represents the different strengths of FOR UPDATE/SHARE
/// clauses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum LockClauseStrength {
    /// No such clause - only used in `PGPlanRowMark` (`PG_LCS_NONE`)
    #[default]
    None = 0,
    /// FOR KEY SHARE (`PG_LCS_FORKEYSHARE`)
    ForKeyShare = 1,
    /// FOR SHARE (`PG_LCS_FORSHARE`)
    ForShare = 2,
    /// FOR NO KEY UPDATE (`PG_LCS_FORNOKEYUPDATE`)
    ForNoKeyUpdate = 3,
    /// `LCS_FORUPDATE`.
    ForUpdate = 4,
}

/// `PGLockWaitPolicy`: This enum controls how to deal with rows being locked by FOR UPDATE/SHARE
/// clauses (`i.e`., it represents the NOWAIT and SKIP LOCKED options)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum LockWaitPolicy {
    /// `PGLockWaitBlock`.
    #[default]
    Block = 0,
    /// `PGLockWaitSkip`.
    Skip = 1,
    /// `LockWaitError`.
    Error = 2,
}

/// `PGMergeAction`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum MergeAction {
    /// WHEN MATCHED (`MERGE_ACTION_WHEN_MATCHED`)
    #[default]
    Matched = 0,
    /// WHEN NOT MATCHED [BY TARGET] (`MERGE_ACTION_WHEN_NOT_MATCHED_BY_TARGET`)
    NotMatchedByTarget = 1,
    /// WHEN NOT MATCHED BY SOURCE (`MERGE_ACTION_WHEN_NOT_MATCHED_BY_SOURCE`)
    NotMatchedBySource = 2,
}

/// `PGMergeActionType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum MergeActionType {
    /// DO UPDATE SET (`MERGE_ACTION_TYPE_UPDATE`)
    #[default]
    Update = 0,
    /// DELETE (`MERGE_ACTION_TYPE_DELETE`)
    Delete = 1,
    /// INSERT (`MERGE_ACTION_TYPE_INSERT`)
    Insert = 2,
    /// DO NOTHING (`MERGE_ACTION_TYPE_DO_NOTHING`)
    DoNothing = 3,
    /// `MERGE_ACTION_TYPE_ERROR`.
    Error = 4,
}

/// `PGNewTypeKind`: Type Statement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum NewTypeKind {
    /// `PG_NEWTYPE_NONE`.
    #[default]
    None = 0,
    /// `PG_NEWTYPE_ENUM`.
    Enum = 1,
    /// `PG_NEWTYPE_ALIAS`.
    Alias = 2,
}

/// `PGNullTestType`: `PGNullTest` `PGNullTest` represents the operation of testing a value for
/// `NULLness`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum NullTestType {
    /// `PG_IS_NULL`.
    #[default]
    Null = 0,
    /// `IS_NOT_NULL`.
    NotNull = 1,
}

/// `PGObjectType`: When a command can act on several kinds of objects with only one parse structure
/// required, use these constants to designate the object type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ObjectType {
    /// `PG_OBJECT_ACCESS_METHOD`.
    #[default]
    AccessMethod = 0,
    /// `PG_OBJECT_AGGREGATE`.
    Aggregate = 1,
    /// `PG_OBJECT_AMOP`.
    Amop = 2,
    /// `PG_OBJECT_AMPROC`.
    Amproc = 3,
    /// Type's attribute, when distinct from column (`PG_OBJECT_ATTRIBUTE`)
    Attribute = 4,
    /// `PG_OBJECT_CAST`.
    Cast = 5,
    /// `PG_OBJECT_COLUMN`.
    Column = 6,
    /// `PG_OBJECT_COLLATION`.
    Collation = 7,
    /// `PG_OBJECT_CONVERSION`.
    Conversion = 8,
    /// `PG_OBJECT_DATABASE`.
    Database = 9,
    /// `PG_OBJECT_DEFAULT`.
    Default = 10,
    /// `PG_OBJECT_DEFACL`.
    Defacl = 11,
    /// `PG_OBJECT_DOMAIN`.
    Domain = 12,
    /// `PG_OBJECT_DOMCONSTRAINT`.
    Domconstraint = 13,
    /// `PG_OBJECT_EVENT_TRIGGER`.
    EventTrigger = 14,
    /// `PG_OBJECT_EXTENSION`.
    Extension = 15,
    /// `PG_OBJECT_FDW`.
    Fdw = 16,
    /// `PG_OBJECT_FOREIGN_SERVER`.
    ForeignServer = 17,
    /// `PG_OBJECT_FOREIGN_TABLE`.
    ForeignTable = 18,
    /// `PG_OBJECT_FUNCTION`.
    Function = 19,
    /// `PG_OBJECT_TABLE_MACRO`.
    TableMacro = 20,
    /// `PG_OBJECT_INDEX`.
    Index = 21,
    /// `PG_OBJECT_LANGUAGE`.
    Language = 22,
    /// `PG_OBJECT_LARGEOBJECT`.
    Largeobject = 23,
    /// `PG_OBJECT_MATVIEW`.
    Matview = 24,
    /// `PG_OBJECT_OPCLASS`.
    Opclass = 25,
    /// `PG_OBJECT_OPERATOR`.
    Operator = 26,
    /// `PG_OBJECT_OPFAMILY`.
    Opfamily = 27,
    /// `PG_OBJECT_POLICY`.
    Policy = 28,
    /// `PG_OBJECT_PUBLICATION`.
    Publication = 29,
    /// `PG_OBJECT_PUBLICATION_REL`.
    PublicationRel = 30,
    /// `PG_OBJECT_ROLE`.
    Role = 31,
    /// `PG_OBJECT_RULE`.
    Rule = 32,
    /// `PG_OBJECT_SCHEMA`.
    Schema = 33,
    /// `PG_OBJECT_SEQUENCE`.
    Sequence = 34,
    /// `PG_OBJECT_SUBSCRIPTION`.
    Subscription = 35,
    /// `PG_OBJECT_STATISTIC_EXT`.
    StatisticExt = 36,
    /// `PG_OBJECT_TABCONSTRAINT`.
    Tabconstraint = 37,
    /// `PG_OBJECT_TABLE`.
    Table = 38,
    /// `PG_OBJECT_TABLESPACE`.
    Tablespace = 39,
    /// `PG_OBJECT_TRANSFORM`.
    Transform = 40,
    /// `PG_OBJECT_TRIGGER`.
    Trigger = 41,
    /// `PG_OBJECT_TSCONFIGURATION`.
    Tsconfiguration = 42,
    /// `PG_OBJECT_TSDICTIONARY`.
    Tsdictionary = 43,
    /// `PG_OBJECT_TSPARSER`.
    Tsparser = 44,
    /// `PG_OBJECT_TSTEMPLATE`.
    Tstemplate = 45,
    /// `PG_OBJECT_TYPE`.
    Type = 46,
    /// `PG_OBJECT_USER_MAPPING`.
    UserMapping = 47,
    /// `PG_OBJECT_VIEW`.
    View = 48,
}

/// `PGOnCommitAction`: What to do at commit time for temporary relations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum OnCommitAction {
    /// No ON COMMIT clause (do nothing) (`PG_ONCOMMIT_NOOP`)
    #[default]
    Noop = 0,
    /// ON COMMIT PRESERVE ROWS (do nothing) (`PG_ONCOMMIT_PRESERVE_ROWS`)
    PreserveRows = 1,
    /// ON COMMIT DELETE ROWS (`PG_ONCOMMIT_DELETE_ROWS`)
    DeleteRows = 2,
    /// `ONCOMMIT_DROP`.
    Drop = 3,
}

/// `PGOnConflictAction`: "ON CONFLICT" clause type of query This is needed in both `parsenodes.h`
/// and `plannodes.h`, so put it here...
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum OnConflictAction {
    /// No "ON CONFLICT" clause (`PG_ONCONFLICT_NONE`)
    #[default]
    None = 0,
    /// ON CONFLICT ... DO NOTHING (`PG_ONCONFLICT_NOTHING`)
    Nothing = 1,
    /// `PG_ONCONFLICT_UPDATE`.
    Update = 2,
}

/// `PGOnConflictActionAlias`: "INSERT OR [REPLACE|IGNORE]" aliases for `OnConflictAction` This is
/// needed in both `parsenodes.h` and `plannodes.h`, so put it here...
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum OnConflictActionAlias {
    /// No "OR [IGNORE|REPLACE]" clause (`PG_ONCONFLICT_ALIAS_NONE`)
    #[default]
    None = 0,
    /// INSERT OR REPLACE (`PG_ONCONFLICT_ALIAS_REPLACE`)
    Replace = 1,
    /// `PG_ONCONFLICT_ALIAS_IGNORE`.
    Ignore = 2,
}

/// `PGOnCreateConflict`: What to do at commit time for temporary relations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum OnCreateConflict {
    /// `PG_ERROR_ON_CONFLICT`.
    #[default]
    Error = 0,
    /// `PG_IGNORE_ON_CONFLICT`.
    Ignore = 1,
    /// `PG_REPLACE_ON_CONFLICT`.
    Replace = 2,
}

/// `PGOverridingKind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum OverridingKind {
    /// `PG_OVERRIDING_NOT_SET`.
    #[default]
    NotSet = 0,
    /// `PG_OVERRIDING_USER_VALUE`.
    UserValue = 1,
    /// `OVERRIDING_SYSTEM_VALUE`.
    SystemValue = 2,
}

/// `PGPragmaKind`: PRAGMA statements Three types of pragma statements: PRAGMA `pragma_name`;
/// (NOTHING) PRAGMA `pragma_name`='param'; (ASSIGNMENT) PRAGMA `pragma_name`('param'); (CALL)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum PragmaKind {
    /// `PG_PRAGMA_TYPE_NOTHING`.
    #[default]
    Nothing = 0,
    /// `PG_PRAGMA_TYPE_ASSIGNMENT`.
    Assignment = 1,
    /// `PG_PRAGMA_TYPE_CALL`.
    Call = 2,
}

/// `PGSetOperation`: Select Statement A "simple" SELECT is represented in the output of `gram.y` by
/// a single `PGSelectStmt` node; so is a VALUES construct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SetOperation {
    /// `PG_SETOP_NONE`.
    #[default]
    None = 0,
    /// `PG_SETOP_UNION`.
    Union = 1,
    /// `PG_SETOP_INTERSECT`.
    Intersect = 2,
    /// `PG_SETOP_EXCEPT`.
    Except = 3,
    /// `PG_SETOP_UNION_BY_NAME`.
    UnionByName = 4,
}

/// `PGSortByDir`: `PGSort` ordering options for ORDER BY and CREATE INDEX.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SortByDir {
    /// `PG_SORTBY_DEFAULT`.
    #[default]
    Default = 0,
    /// `PG_SORTBY_ASC`.
    Asc = 1,
    /// `PG_SORTBY_DESC`.
    Desc = 2,
    /// `SORTBY_USING`.
    Using = 3,
}

/// `PGSortByNulls`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SortByNulls {
    /// `PG_SORTBY_NULLS_DEFAULT`.
    #[default]
    Default = 0,
    /// `PG_SORTBY_NULLS_FIRST`.
    First = 1,
    /// `PG_SORTBY_NULLS_LAST`.
    Last = 2,
}

/// `PGSubLinkType`: `PGSubLink` A `PGSubLink` represents a subselect appearing in an expression,
/// and in some cases also the combining operator(s) just above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SubLinkType {
    /// `PG_EXISTS_SUBLINK`.
    #[default]
    Exists = 0,
    /// `PG_ALL_SUBLINK`.
    All = 1,
    /// `PG_ANY_SUBLINK`.
    Any = 2,
    /// `PG_ROWCOMPARE_SUBLINK`.
    RowCompare = 3,
    /// `PG_EXPR_SUBLINK`.
    Expr = 4,
    /// `PG_MULTIEXPR_SUBLINK`.
    MultiExpr = 5,
    /// `PG_ARRAY_SUBLINK`.
    Array = 6,
    /// `PG_CTE_SUBLINK`.
    Cte = 7,
}

/// `PGTransactionStmtKind`: {Begin|Commit|Rollback} Transaction Statement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TransactionStmtKind {
    /// `PG_TRANS_STMT_BEGIN`.
    #[default]
    Begin = 0,
    /// Semantically identical to BEGIN (`PG_TRANS_STMT_START`)
    Start = 1,
    /// `PG_TRANS_STMT_COMMIT`.
    Commit = 2,
    /// `PG_TRANS_STMT_ROLLBACK`.
    Rollback = 3,
    /// `PG_TRANS_STMT_SAVEPOINT`.
    Savepoint = 4,
    /// `PG_TRANS_STMT_RELEASE`.
    Release = 5,
    /// `PG_TRANS_STMT_ROLLBACK_TO`.
    RollbackTo = 6,
    /// `PG_TRANS_STMT_PREPARE`.
    Prepare = 7,
    /// `PG_TRANS_STMT_COMMIT_PREPARED`.
    CommitPrepared = 8,
    /// `TRANS_STMT_ROLLBACK_PREPARED`.
    RollbackPrepared = 9,
}

/// `PGTransactionStmtType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum TransactionStmtType {
    /// `PG_TRANS_TYPE_DEFAULT`.
    #[default]
    Default = 0,
    /// Explicit READ ONLY (`PG_TRANS_TYPE_READ_ONLY`)
    ReadOnly = 1,
    /// `PG_TRANS_TYPE_READ_WRITE`.
    ReadWrite = 2,
}

/// `PGViewCheckOption`: Create View Statement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ViewCheckOption {
    /// `PG_NO_CHECK_OPTION`.
    #[default]
    None = 0,
    /// `PG_LOCAL_CHECK_OPTION`.
    Local = 1,
    /// `CASCADED_CHECK_OPTION`.
    Cascaded = 2,
}

/// `VariableSetKind`: SET Statement (includes RESET) "SET var TO DEFAULT" and "RESET var" are
/// semantically equivalent, but we preserve the distinction in `VariableSetKind` for
/// `CreateCommandTag()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum VariableSetKind {
    /// SET var = value (`VAR_SET_VALUE`)
    #[default]
    Value = 0,
    /// SET var TO DEFAULT (`VAR_SET_DEFAULT`)
    Default = 1,
    /// SET var FROM CURRENT (`VAR_SET_CURRENT`)
    Current = 2,
    /// Special case for SET TRANSACTION ... (`VAR_SET_MULTI`)
    Multi = 3,
    /// RESET var (`VAR_RESET`)
    Reset = 4,
    /// RESET ALL (`VAR_RESET_ALL`)
    ResetAll = 5,
}

/// `VariableSetScope`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum VariableSetScope {
    /// SET LOCAL var (`VAR_SET_SCOPE_LOCAL`)
    #[default]
    Local = 0,
    /// SET SESSION var (`VAR_SET_SCOPE_SESSION`)
    Session = 1,
    /// SET GLOBAL var (`VAR_SET_SCOPE_GLOBAL`)
    Global = 2,
    /// SET VARIABLE var (`VAR_SET_SCOPE_VARIABLE`)
    Variable = 3,
    /// `VAR_SET_SCOPE_DEFAULT`.
    Default = 4,
}

/// `PGPostgresRelPersistence`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum RelPersistence {
    /// `PG_RELPERSISTENCE_TEMP`.
    #[default]
    Temp = 0,
    /// `PG_RELPERSISTENCE_UNLOGGED`.
    Unlogged = 1,
    /// `RELPERSISTENCE_PERMANENT`.
    Permanent = 2,
}
