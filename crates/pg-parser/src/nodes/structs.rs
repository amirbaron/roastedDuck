//! The node structs of the Postgres parse tree.
//!
//! Field names follow libpg_query's C structs (`targetList` becomes `target_list`).
//! Pointers become `Option`, lists become [`List`], C strings become `Option<String>`.

use super::{
    AExprKind, AlterDatabaseType, AlterTableType, BoolExprType, BoolTestType, ColumnCategory, ConstrType,
    CteMaterialize, DefElemAction, DropBehavior, GroupingSetKind, IgnoreNulls, InsertColumnOrder,
    JoinRefType, JoinType, List, LoadInstallType, LockClauseStrength, LockWaitPolicy, MergeAction,
    MergeActionType, NewTypeKind, NullTestType, ObjectType, OnCommitAction, OnConflictAction,
    OnConflictActionAlias, OnCreateConflict, OverridingKind, PragmaKind, RelPersistence, SetOperation,
    SortByDir, SortByNulls, SubLinkType, TransactionStmtKind, TransactionStmtType, Value, VariableSetKind,
    VariableSetScope, ViewCheckOption,
};

nodes! {
    /// 'PGRawStmt': container for any one statement's raw parse tree Parse analysis converts a raw
    /// parse tree headed by a PGRawStmt node into an analyzed statement headed by a PGQuery node.
    RawStmt {
        /// Raw parse tree.
        stmt: Option<Node>,
        /// Start location, or -1 if unknown.
        stmt_location: i32,
        /// Length in bytes; 0 means "rest of string".
        stmt_len: i32,
    }
    /// 'PGSelectStmt': The 'PGSelectStmt' node.
    SelectStmt {
        /// NULL, list of DISTINCT ON exprs, or lcons(NIL,NIL) for all (SELECT DISTINCT)
        distinct_clause: Option<List>,
        /// Target for SELECT INTO.
        into_clause: Option<Box<IntoClause>>,
        /// The target list (of PGResTarget)
        target_list: List,
        /// The FROM clause.
        from_clause: List,
        /// WHERE qualification.
        where_clause: Option<Node>,
        /// GROUP BY clauses.
        group_clause: List,
        /// HAVING conditional-expression.
        having_clause: Option<Node>,
        /// WINDOW window_name AS (...), ...
        window_clause: List,
        /// QUALIFY conditional-expression.
        qualify_clause: Option<Node>,
        /// Untransformed list of expression lists.
        values_lists: List,
        /// PIVOT statement.
        pivot: Option<Box<PivotStmt>>,
        /// Sort clause (a list of SortBy's)
        sort_clause: List,
        /// # of result tuples to skip.
        limit_offset: Option<Node>,
        /// # of result tuples to return.
        limit_count: Option<Node>,
        /// Sample options (if any)
        sample_options: Option<Node>,
        /// FOR UPDATE (list of LockingClause's)
        locking_clause: List,
        /// WITH clause.
        with_clause: Option<Box<WithClause>>,
        /// Type of set op.
        op: SetOperation,
        /// ALL specified?
        all: bool,
        /// FROM first or SELECT first.
        from_first: bool,
        /// OFFSET first or LIMIT first.
        offset_first: bool,
        /// Left child.
        larg: Option<Node>,
        /// Right child.
        rarg: Option<Node>,
    }
    /// 'PGInsertStmt': Insert Statement The source expression is represented by PGSelectStmt for
    /// both the SELECT and VALUES cases.
    InsertStmt {
        /// Relation to insert into.
        relation: Option<Box<RangeVar>>,
        /// Optional: names of the target columns.
        cols: List,
        /// The source SELECT/VALUES, or NULL.
        select_stmt: Option<Node>,
        /// The (optional) shorthand provided for the onConflictClause.
        on_conflict_alias: OnConflictActionAlias,
        /// ON CONFLICT clause.
        on_conflict_clause: Option<Box<OnConflictClause>>,
        /// List of expressions to return.
        returning_list: List,
        /// WITH clause.
        with_clause: Option<Box<WithClause>>,
        /// OVERRIDING clause.
        overriding: OverridingKind,
        /// INSERT BY NAME or INSERT BY POSITION.
        insert_column_order: InsertColumnOrder,
    }
    /// 'PGUpdateStmt': Update Statement.
    UpdateStmt {
        /// Relation to update.
        relation: Option<Box<RangeVar>>,
        /// The target list (of PGResTarget)
        target_list: List,
        /// Qualifications.
        where_clause: Option<Node>,
        /// Optional from clause for more tables.
        from_clause: List,
        /// List of expressions to return.
        returning_list: List,
        /// WITH clause.
        with_clause: Option<Box<WithClause>>,
    }
    /// 'PGDeleteStmt': Delete Statement.
    DeleteStmt {
        /// Relation to delete from.
        relation: Option<Box<RangeVar>>,
        /// Optional using clause for more tables.
        using_clause: List,
        /// Qualifications.
        where_clause: Option<Node>,
        /// List of expressions to return.
        returning_list: List,
        /// WITH clause.
        with_clause: Option<Box<WithClause>>,
    }
    /// 'PGMergeIntoStmt': Merge Into Statement.
    MergeIntoStmt {
        /// Relation to merge into.
        target_table: Option<Box<RangeVar>>,
        /// Source table or query.
        source: Option<Node>,
        /// Qualifications.
        join_condition: Option<Node>,
        /// Optional using clause for more tables.
        using_clause: List,
        /// List of match actions.
        match_actions: List,
        /// WITH clause.
        with_clause: Option<Box<WithClause>>,
        /// Return-values list (of PGTargetEntry)
        returning_list: List,
    }
    /// 'PGMatchAction': The 'PGMatchAction' node.
    MatchAction {
        /// WHEN MATCHED or WHEN NOT MATCHED.
        when: MergeAction,
        /// AND condition, if any.
        and_clause: Option<Node>,
        /// UPDATE, DELETE, etc.
        action_type: MergeActionType,
        /// The target list for UPDATE.
        update_targets: List,
        /// BY NAME or BY POSITION.
        insert_column_order: InsertColumnOrder,
        /// Optional: names of the target columns for insert.
        insert_cols: List,
        /// Values for insert.
        insert_values: List,
        /// Expression to generate the error message, if any.
        error_message: Option<Node>,
        /// DEFAULT VALUES.
        default_values: bool,
    }
    /// 'PGCreateStmt': Create Table Statement NOTE: in the raw gram.y output, PGColumnDef and
    /// PGConstraint nodes are intermixed in tableElts, and constraints is NIL.
    CreateStmt {
        /// Relation to create.
        relation: Option<Box<RangeVar>>,
        /// Column definitions (list of PGColumnDef)
        table_elts: List,
        /// Relations to inherit from (list of inhRelation)
        inh_relations: List,
        /// Constraints (list of PGConstraint nodes)
        constraints: List,
        /// Options from WITH clause.
        options: List,
        /// E.g. expression list for partitioned by.
        partition_list: List,
        /// E.g. expression list for sort by.
        sort_list: List,
        /// What do we do at COMMIT?
        oncommit: OnCommitAction,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGCreateTableAsStmt': CREATE TABLE AS Statement (a/k/a SELECT INTO) A query written as
    /// CREATE TABLE AS will produce this node type natively.
    CreateTableAsStmt {
        /// The query (see comments above)
        query: Option<Node>,
        /// Destination table.
        into: Option<Box<IntoClause>>,
        /// PG_OBJECT_TABLE or PG_OBJECT_MATVIEW.
        relkind: ObjectType,
        /// It was written as SELECT INTO.
        is_select_into: bool,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGCreateSchemaStmt': Create Schema Statement NOTE: the schemaElts list contains raw
    /// parsetrees for component statements of the schema, such as CREATE TABLE, GRANT, etc.
    CreateSchemaStmt {
        /// The name of the catalog in which to create the schema.
        catalogname: Option<String>,
        /// The name of the schema to create.
        schemaname: Option<String>,
        /// Schema components (list of parsenodes)
        schema_elts: List,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGCreateSeqStmt': {Create|Alter} SEQUENCE Statement.
    CreateSeqStmt {
        /// The sequence to create.
        sequence: Option<Box<RangeVar>>,
        /// 'options'.
        options: List,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGAlterSeqStmt': The 'PGAlterSeqStmt' node.
    AlterSeqStmt {
        /// The sequence to alter.
        sequence: Option<Box<RangeVar>>,
        /// 'options'.
        options: List,
        /// Skip error if a role is missing?
        missing_ok: bool,
    }
    /// 'PGCreateFunctionStmt': The 'PGCreateFunctionStmt' node.
    CreateFunctionStmt {
        /// 'name'.
        name: Option<Box<RangeVar>>,
        /// 'functions'.
        functions: List,
        /// 'onconflict'.
        onconflict: OnCreateConflict,
    }
    /// 'PGFunctionDefinition': CREATE FUNCTION Statement.
    FunctionDefinition {
        /// 'params'.
        params: List,
        /// 'function'.
        function: Option<Node>,
        /// 'query'.
        query: Option<Node>,
    }
    /// 'PGFunctionParameter': Function Parameter.
    FunctionParameter {
        /// Name of parameter.
        name: Option<String>,
        /// Type of parameter (optional)
        type_name: Option<Box<TypeName>>,
        /// Default value of parameter (optional)
        default_value: Option<Node>,
    }
    /// 'PGCreateTypeStmt': The 'PGCreateTypeStmt' node.
    CreateTypeStmt {
        /// 'kind'.
        kind: NewTypeKind,
        /// Qualified name (list of Value strings)
        type_name: Option<Box<RangeVar>>,
        /// Enum values (list of Value strings)
        vals: List,
        /// Original type of alias name.
        of_type: Option<Box<TypeName>>,
        /// 'query'.
        query: Option<Node>,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGCreateSecretStmt': Create Secret Statement.
    CreateSecretStmt {
        /// The requested persist mode.
        persist_type: Option<String>,
        /// Name of the secret.
        secret_name: Option<String>,
        /// The optional storage type of the secret.
        secret_storage: Option<String>,
        /// Optionally the scopes of the secret.
        scope: List,
        /// Secret options.
        options: List,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGDropSecretStmt': Drop Secret Statement.
    DropSecretStmt {
        /// The requested persist mode.
        persist_type: Option<String>,
        /// Name of the secret.
        secret_name: Option<String>,
        /// 'secret_storage'.
        secret_storage: Option<String>,
        /// 'missing_ok'.
        missing_ok: bool,
    }
    /// 'PGViewStmt': The 'PGViewStmt' node.
    ViewStmt {
        /// The view to be created.
        view: Option<Box<RangeVar>>,
        /// Target column names.
        aliases: List,
        /// The SELECT query (as a raw parse tree)
        query: Option<Node>,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
        /// Options from WITH clause.
        options: List,
        /// WITH CHECK OPTION.
        with_check_option: ViewCheckOption,
    }
    /// 'PGIndexStmt': Create PGIndex Statement This represents creation of an index and/or an
    /// associated constraint.
    IndexStmt {
        /// Name of new index, or NULL for default.
        idxname: Option<String>,
        /// Relation to build index on.
        relation: Option<Box<RangeVar>>,
        /// Name of access method (eg. btree)
        access_method: Option<String>,
        /// Columns to index: a list of PGIndexElem.
        index_params: List,
        /// WITH clause options: a list of PGDefElem.
        options: List,
        /// Qualification (partial-index predicate)
        where_clause: Option<Node>,
        /// Is index unique?
        unique: bool,
        /// Should this be a concurrent index build?
        concurrent: bool,
        /// What to do on create conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGAlterTableStmt': Alter Table.
    AlterTableStmt {
        /// Table to work on.
        relation: Option<Box<RangeVar>>,
        /// List of subcommands.
        cmds: List,
        /// Type of object.
        relkind: ObjectType,
        /// Skip error if table missing.
        missing_ok: bool,
    }
    /// 'PGAlterTableCmd': The 'PGAlterTableCmd' node.
    AlterTableCmd {
        /// Type of table alteration to apply.
        subtype: AlterTableType,
        /// Column, constraint, or trigger to act on, or tablespace.
        name: Option<String>,
        /// Definition of new column, index, * constraint, or parent table.
        def: Option<Node>,
        /// E.g. expression list for partitioned by or sorted by.
        def_list: List,
        /// Set table options e.g. SET ('foo'='bar'); RESET ('foo'='bar')
        options: List,
        /// RESTRICT or CASCADE for DROP cases.
        behavior: DropBehavior,
        /// Skip error if missing?
        missing_ok: bool,
    }
    /// 'PGAlterObjectSchemaStmt': ALTER object SET SCHEMA Statement.
    AlterObjectSchemaStmt {
        /// PG_OBJECT_TABLE, PG_OBJECT_TYPE, etc.
        object_type: ObjectType,
        /// In case it's a table.
        relation: Option<Box<RangeVar>>,
        /// The new schema.
        newschema: Option<String>,
        /// Skip error if missing?
        missing_ok: bool,
    }
    /// 'PGAlterDatabaseStmt': The 'PGAlterDatabaseStmt' node.
    AlterDatabaseStmt {
        /// 'dbname'.
        dbname: Option<String>,
        /// 'new_name'.
        new_name: Option<String>,
        /// 'alter_type'.
        alter_type: AlterDatabaseType,
        /// 'missing_ok'.
        missing_ok: bool,
    }
    /// 'PGRenameStmt': Alter Object Rename Statement.
    RenameStmt {
        /// PG_OBJECT_TABLE, PG_OBJECT_COLUMN, etc.
        rename_type: ObjectType,
        /// If column name, associated relation type.
        relation_type: ObjectType,
        /// In case it's a table.
        relation: Option<Box<RangeVar>>,
        /// Name of contained object (column, rule, trigger, etc)
        subname: Option<String>,
        /// Names of contained object (e.g. qualified column)
        name_list: List,
        /// The new name.
        newname: Option<String>,
        /// RESTRICT or CASCADE behavior.
        behavior: DropBehavior,
        /// Skip error if missing?
        missing_ok: bool,
    }
    /// 'PGDropStmt': Drop Table|Sequence|View|Index|Type|Domain|Conversion|Schema Statement.
    DropStmt {
        /// List of names.
        objects: List,
        /// Object type.
        remove_type: ObjectType,
        /// RESTRICT or CASCADE behavior.
        behavior: DropBehavior,
        /// Skip error if object is missing?
        missing_ok: bool,
    }
    /// 'PGCommentOnStmt': Comment On Statement.
    CommentOnStmt {
        /// Object type.
        object_type: ObjectType,
        /// The object to comment on.
        name: Option<Box<RangeVar>>,
        /// The comment: a string or NULL.
        value: Option<Node>,
        /// 'column_expr'.
        column_expr: Option<Node>,
    }
    /// 'PGCopyStmt': Copy Statement We support "COPY relation FROM file", "COPY relation TO file",
    /// and "COPY (query) TO file".
    CopyStmt {
        /// The relation to copy.
        relation: Option<Box<RangeVar>>,
        /// The query (SELECT or DML statement with RETURNING) to copy, as a raw parse tree.
        query: Option<Node>,
        /// PGList of column names (as Strings), or NIL for all columns.
        attlist: List,
        /// TO or FROM.
        is_from: bool,
        /// Is 'filename' a program to popen?
        is_program: bool,
        /// Filename.
        filename: Option<Node>,
        /// PGList of PGDefElem nodes.
        options: List,
    }
    /// 'PGCopyDatabaseStmt': Copy Database Statement.
    CopyDatabaseStmt {
        /// 'from_database'.
        from_database: Option<String>,
        /// 'to_database'.
        to_database: Option<String>,
        /// 'copy_database_flag'.
        copy_database_flag: Option<String>,
    }
    /// 'PGExplainStmt': Explain Statement The "query" field is initially a raw parse tree, and is
    /// converted to a PGQuery node during parse analysis.
    ExplainStmt {
        /// The query (see comments above)
        query: Option<Node>,
        /// List of PGDefElem nodes.
        options: List,
    }
    /// 'PGExportStmt': EXPORT/IMPORT Statements.
    ExportStmt {
        /// Database name.
        database: Option<String>,
        /// Filename.
        filename: Option<String>,
        /// PGList of PGDefElem nodes.
        options: List,
    }
    /// 'PGImportStmt': The 'PGImportStmt' node.
    ImportStmt {
        /// Filename.
        filename: Option<String>,
    }
    /// 'PGPrepareStmt': PREPARE Statement.
    PrepareStmt {
        /// Name of plan, arbitrary.
        name: Option<String>,
        /// Types of parameters (PGList of PGTypeName)
        argtypes: List,
        /// The query itself (as a raw parsetree)
        query: Option<Node>,
    }
    /// 'PGExecuteStmt': EXECUTE Statement.
    ExecuteStmt {
        /// The name of the plan to execute.
        name: Option<String>,
        /// Values to assign to parameters.
        params: List,
    }
    /// 'PGDeallocateStmt': DEALLOCATE Statement.
    DeallocateStmt {
        /// The name of the plan to remove.
        name: Option<String>,
    }
    /// 'PGCallStmt': CALL Statement.
    CallStmt {
        /// 'func'.
        func: Option<Node>,
    }
    /// 'PGPragmaStmt': The 'PGPragmaStmt' node.
    PragmaStmt {
        /// 'kind'.
        kind: PragmaKind,
        /// Variable to be set.
        name: Option<String>,
        /// PGList of PGAConst nodes.
        args: List,
    }
    /// 'PGTransactionStmt': The 'PGTransactionStmt' node.
    TransactionStmt {
        /// See above.
        kind: TransactionStmtKind,
        /// For BEGIN/START and savepoint commands.
        options: List,
        /// Read only or read write.
        transaction_type: TransactionStmtType,
    }
    /// 'PGVariableSetStmt': The 'PGVariableSetStmt' node.
    VariableSetStmt {
        /// 'kind'.
        kind: VariableSetKind,
        /// 'scope'.
        scope: VariableSetScope,
        /// Variable to be set.
        name: Option<String>,
        /// PGList of PGAConst nodes.
        args: List,
    }
    /// 'PGVariableShowStmt': Show Statement.
    VariableShowStmt {
        /// Relation to describe (if any)
        relation: Option<Box<RangeVar>>,
        /// Set to describe (e.g. set when using SHOW ALL TABLES)
        set: Option<String>,
        /// Whether or not this is a DESCRIBE or a SUMMARIZE.
        is_summary: i32,
    }
    /// 'PGVariableShowSelectStmt': Show Statement with Select Statement.
    VariableShowSelectStmt {
        /// 'stmt'.
        stmt: Option<Node>,
        /// 'name'.
        name: Option<String>,
        /// Whether or not this is a DESCRIBE or a SUMMARIZE.
        is_summary: i32,
    }
    /// 'PGVacuumStmt': The 'PGVacuumStmt' node.
    VacuumStmt {
        /// OR of PGVacuumOption flags.
        options: i32,
        /// Single table to process, or NULL.
        relation: Option<Box<RangeVar>>,
        /// List of column names, or NIL for all.
        va_cols: List,
    }
    /// 'PGLoadStmt': The 'PGLoadStmt' node.
    LoadStmt {
        /// File to load.
        filename: Option<String>,
        /// Optionally, the repository to load from.
        repository: Option<String>,
        /// Whether the repository was passed as an alias or a raw path.
        repo_is_alias: bool,
        /// Optionally, the version of the extension to be loaded.
        version: Option<String>,
        /// 'load_type'.
        load_type: LoadInstallType,
    }
    /// 'PGUpdateExtensionsStmt': Update Extensions Statement.
    UpdateExtensionsStmt {
        /// 'extensions'.
        extensions: List,
    }
    /// 'PGAttachStmt': Attach Statement.
    AttachStmt {
        /// The file path of the to-be-attached database.
        path: Option<String>,
        /// The name of the attached database.
        name: Option<String>,
        /// PGList of PGDefElem nodes.
        options: List,
        /// 'query'.
        query: Option<Node>,
        /// What to do on attach conflict.
        onconflict: OnCreateConflict,
    }
    /// 'PGDetachStmt': Dettach Statement.
    DetachStmt {
        /// List of names of attached databases.
        db_name: Option<String>,
        /// 'missing_ok'.
        missing_ok: bool,
    }
    /// 'PGUseStmt': Use Statement.
    UseStmt {
        /// Variable to be set.
        name: Option<Box<RangeVar>>,
    }
    /// 'PGCheckPointStmt': Checkpoint Statement.
    CheckPointStmt {
        /// 'force'.
        force: bool,
        /// 'name'.
        name: Option<String>,
    }
    /// 'PGPivotStmt': The 'PGPivotStmt' node.
    PivotStmt {
        /// The source to pivot.
        source: Option<Node>,
        /// The aggregations to pivot over (PIVOT only)
        aggrs: List,
        /// The names to unpivot over (UNPIVOT only)
        unpivots: List,
        /// The set of columns to pivot over.
        columns: List,
        /// The set of groups to pivot over (if any)
        groups: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGAlias': specifies an alias for a range variable; the alias might also specify renaming of
    /// columns within the table.
    Alias {
        /// Aliased rel name (never qualified)
        aliasname: Option<String>,
        /// Optional list of column aliases.
        colnames: List,
    }
    /// 'PGRangeVar': range variable, used in FROM clauses Also used to represent table names in
    /// utility statements; there, the alias field is not used, and inh tells whether to apply the
    /// operation recursively to child tables.
    RangeVar {
        /// The catalog (database) name, or NULL.
        catalogname: Option<String>,
        /// The schema name, or NULL.
        schemaname: Option<String>,
        /// The relation/sequence name.
        relname: Option<String>,
        /// Expand rel by inheritance? recursively act on children?
        inh: bool,
        /// See RELPERSISTENCE_* in pg_class.h.
        relpersistence: RelPersistence,
        /// Version of the table to read.
        at_clause: Option<Node>,
        /// Table alias & optional column aliases.
        alias: Option<Box<Alias>>,
        /// Token location, or -1 if unknown.
        location: i32,
        /// Sample, if any.
        sample: Option<Node>,
    }
    /// 'PGRangeSubselect': subquery appearing in a FROM clause.
    RangeSubselect {
        /// Does it have LATERAL prefix?
        lateral: bool,
        /// The untransformed sub-select clause.
        subquery: Option<Node>,
        /// Table alias & optional column aliases.
        alias: Option<Box<Alias>>,
        /// Sample options (if any)
        sample: Option<Node>,
    }
    /// 'PGRangeFunction': function call appearing in a FROM clause functions is a PGList because we
    /// use this to represent the construct ROWS FROM(func1(...), func2(...), ...)
    RangeFunction {
        /// Does it have LATERAL prefix?
        lateral: bool,
        /// Does it have WITH ORDINALITY suffix?
        ordinality: bool,
        /// Is result of ROWS FROM() syntax?
        is_rowsfrom: bool,
        /// Per-function information, see above.
        functions: List,
        /// Table alias & optional column aliases.
        alias: Option<Box<Alias>>,
        /// List of PGColumnDef nodes to describe result of function returning RECORD.
        coldeflist: List,
        /// Sample options (if any)
        sample: Option<Node>,
    }
    /// 'PGJoinExpr': for SQL JOIN expressions joinreftype, usingClause, and quals are
    /// interdependent.
    JoinExpr {
        /// Type of join.
        jointype: JoinType,
        /// Regular/Natural/AsOf join? Will need to shape table.
        joinreftype: JoinRefType,
        /// Left subtree.
        larg: Option<Node>,
        /// Right subtree.
        rarg: Option<Node>,
        /// USING clause, if any (list of String)
        using_clause: List,
        /// Qualifiers on join, if any.
        quals: Option<Node>,
        /// User-written alias clause, if any.
        alias: Option<Box<Alias>>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGWithClause': representation of WITH clause Note: PGWithClause does not propagate into the
    /// PGQuery representation; but PGCommonTableExpr does.
    WithClause {
        /// List of CommonTableExprs.
        ctes: List,
        /// True = WITH RECURSIVE.
        recursive: bool,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGCommonTableExpr': The 'PGCommonTableExpr' node.
    CommonTableExpr {
        /// Query name (never qualified)
        ctename: Option<String>,
        /// Optional list of column names.
        aliascolnames: List,
        /// Is this an optimization fence?
        ctematerialized: CteMaterialize,
        /// The CTE's subquery.
        ctequery: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
        /// Is this CTE actually recursive?
        cterecursive: bool,
        /// 'recursive_keys'.
        recursive_keys: List,
        /// List of output column names.
        ctecolnames: List,
        /// OID list of output column type OIDs.
        ctecoltypes: List,
        /// Integer list of output column typmods.
        ctecoltypmods: List,
        /// OID list of column collation OIDs.
        ctecolcollations: List,
    }
    /// 'PGIntoClause': target information for SELECT INTO, CREATE TABLE AS, and CREATE MATERIALIZED
    /// VIEW For CREATE MATERIALIZED VIEW, viewQuery is the parsed-but-not-rewritten SELECT PGQuery
    /// for the view; otherwise it's NULL.
    IntoClause {
        /// Target relation name.
        rel: Option<Box<RangeVar>>,
        /// Column names to assign, or NIL.
        col_names: List,
        /// Options from WITH clause.
        options: List,
        /// E.g. expression list for partitioned by.
        partition_list: List,
        /// E.g. expression list for sort by.
        sort_list: List,
        /// What do we do at COMMIT?
        on_commit: OnCommitAction,
        /// True for WITH NO DATA.
        skip_data: bool,
    }
    /// 'PGOnConflictClause': representation of ON CONFLICT clause Note: PGOnConflictClause does not
    /// propagate into the PGQuery representation.
    OnConflictClause {
        /// DO NOTHING or UPDATE?
        action: OnConflictAction,
        /// Optional index inference clause.
        infer: Option<Box<InferClause>>,
        /// The target list (of PGResTarget)
        target_list: List,
        /// Qualifications.
        where_clause: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGInferClause': ON CONFLICT unique index inference clause Note: PGInferClause does not
    /// propagate into the PGQuery representation.
    InferClause {
        /// IndexElems to infer unique index.
        index_elems: List,
        /// Qualification (partial-index predicate)
        where_clause: Option<Node>,
        /// PGConstraint name, or NULL if unnamed.
        conname: Option<String>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGIndexElem': index parameters (used in CREATE INDEX, and in ON CONFLICT) For a plain index
    /// attribute, 'name' is the name of the table column to index, and 'expr' is NULL.
    IndexElem {
        /// Name of attribute to index, or NULL.
        name: Option<String>,
        /// Expression to index, or NULL.
        expr: Option<Node>,
        /// Name of collation; NIL = default.
        collation: List,
        /// Name of desired opclass; NIL = default.
        opclass: List,
        /// ASC/DESC/default.
        ordering: SortByDir,
        /// FIRST/LAST/default.
        nulls_ordering: SortByNulls,
    }
    /// 'PGLockingClause': raw representation of FOR [NO KEY] UPDATE/[KEY] SHARE options Note:
    /// lockedRels == NIL means "all relations in query".
    LockingClause {
        /// FOR [KEY] UPDATE/SHARE relations.
        locked_rels: List,
        /// 'strength'.
        strength: LockClauseStrength,
        /// NOWAIT and SKIP LOCKED.
        wait_policy: LockWaitPolicy,
    }
    /// 'PGSortBy': for ORDER BY clause.
    SortBy {
        /// Expression to sort on.
        node: Option<Node>,
        /// ASC/DESC/USING/default.
        sortby_dir: SortByDir,
        /// NULLS FIRST/LAST.
        sortby_nulls: SortByNulls,
        /// Name of op to use, if SORTBY_USING.
        use_op: List,
        /// Operator location, or -1 if none/unknown.
        location: i32,
    }
    /// 'PGWindowDef': raw representation of WINDOW and OVER clauses For entries in a WINDOW list,
    /// "name" is the window name being defined.
    WindowDef {
        /// Window's own name.
        name: Option<String>,
        /// Referenced window name, if any.
        refname: Option<String>,
        /// PARTITION BY expression list.
        partition_clause: List,
        /// ORDER BY (list of PGSortBy)
        order_clause: List,
        /// Frame_clause options, see below.
        frame_options: i32,
        /// Expression for starting bound, if any.
        start_offset: Option<Node>,
        /// Expression for ending bound, if any.
        end_offset: Option<Node>,
        /// Parse location, or -1 if none/unknown.
        location: i32,
    }
    /// 'PGGroupingSet': The 'PGGroupingSet' node.
    GroupingSet {
        /// 'kind'.
        kind: GroupingSetKind,
        /// 'content'.
        content: List,
        /// 'location'.
        location: i32,
    }
    /// 'PGSampleSize': Sample Options.
    SampleSize {
        /// Whether or not the sample size is expressed in row numbers or a percentage.
        is_percentage: bool,
        /// Sample size.
        sample_size: Option<Node>,
    }
    /// 'PGSampleOptions': The 'PGSampleOptions' node.
    SampleOptions {
        /// The size of the sample to take.
        sample_size: Option<Node>,
        /// Sample method, or NULL for default.
        method: Option<String>,
        /// If the sample method has seed.
        has_seed: bool,
        /// The seed value if set;.
        seed: i32,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGLimitPercent': Limit Percentage.
    LimitPercent {
        /// Limit percent.
        limit_percent: Option<Node>,
    }
    /// 'PGAtClause': AT clause in FROM tables - e.g.
    AtClause {
        /// The unit (e.g. TIMESTAMP/VERSION)
        unit: Option<String>,
        /// The expression.
        expr: Option<Node>,
    }
    /// 'PGPivot': Pivot Expression.
    Pivot {
        /// The column names to pivot on.
        pivot_columns: List,
        /// The column names to unpivot.
        unpivot_columns: List,
        /// The set of pivot values.
        pivot_value: List,
        /// Subquery to fetch valid pivot values (if any)
        subquery: Option<Node>,
        /// The enum to fetch the unique values from.
        pivot_enum: Option<String>,
    }
    /// 'PGPivotExpr': The 'PGPivotExpr' node.
    PivotExpr {
        /// The source subtree.
        source: Option<Node>,
        /// The aggregations to pivot over (PIVOT only)
        aggrs: List,
        /// The names to unpivot over (UNPIVOT only)
        unpivots: List,
        /// The set of pivot values.
        pivots: List,
        /// The set of groups to pivot over (if any)
        groups: List,
        /// Table alias & optional column aliases.
        alias: Option<Box<Alias>>,
        /// Whether or not to include NULL values (UNPIVOT only.
        include_nulls: bool,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGResTarget': result target (used in target list of pre-transformed parse trees) In a
    /// SELECT target list, 'name' is the column label from an 'AS ColumnLabel' clause, or NULL if
    /// there was none, and 'val' is the value expression itself.
    ResTarget {
        /// Column name or NULL.
        name: Option<String>,
        /// Subscripts, field names, and '*', or NIL.
        indirection: List,
        /// The value expression to compute or assign.
        val: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGMultiAssignRef': element of a row source expression for UPDATE In an UPDATE target list,
    /// when we have SET (a,b,c) = row-valued-expression, we generate separate PGResTarget items for
    /// each of a,b,c.
    MultiAssignRef {
        /// The row-valued expression.
        source: Option<Node>,
        /// Column number for this target (1..n)
        colno: i32,
        /// Number of targets in the construct.
        ncolumns: i32,
    }
    /// 'PGDefElem': The 'PGDefElem' node.
    DefElem {
        /// NULL if unqualified name.
        defnamespace: Option<String>,
        /// 'defname'.
        defname: Option<String>,
        /// A (PGValue *) or a (PGTypeName *)
        arg: Option<Node>,
        /// Unspecified action, or SET/ADD/DROP.
        defaction: DefElemAction,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGColumnDef': column definition (used in various creates) If the column has a default
    /// value, we may have the value expression in either "raw" form (an untransformed parse tree)
    /// or "cooked" form (a post-parse-analysis, executable expression tree), depending on how this
    /// PGColumnDef node was created (by parsing, or by inheritance from an existing relation)
    ColumnDef {
        /// Name of column.
        colname: Option<String>,
        /// Type of column.
        type_name: Option<Box<TypeName>>,
        /// Default value (untransformed parse tree)
        raw_default: Option<Node>,
        /// Untransformed COLLATE spec, if any.
        coll_clause: Option<Box<CollateClause>>,
        /// Other constraints on column.
        constraints: List,
        /// Parse location, or -1 if none/unknown.
        location: i32,
        /// Category of the column.
        category: ColumnCategory,
    }
    /// 'PGConstraint': The 'PGConstraint' node.
    Constraint {
        /// See above.
        contype: ConstrType,
        /// PGConstraint name, or NULL if unnamed.
        conname: Option<String>,
        /// DEFERRABLE?
        deferrable: bool,
        /// INITIALLY DEFERRED?
        initdeferred: bool,
        /// Token location, or -1 if unknown.
        location: i32,
        /// Is constraint non-inheritable?
        is_no_inherit: bool,
        /// Expr, as untransformed parse tree.
        raw_expr: Option<Node>,
        /// 'generated_when'.
        generated_when: u8,
        /// String nodes naming referenced column(s)
        keys: List,
        /// Options from WITH clause.
        options: List,
        /// Existing index to use; otherwise NULL.
        indexname: Option<String>,
        /// Partial index predicate.
        where_clause: Option<Node>,
        /// Primary key table.
        pktable: Option<Box<RangeVar>>,
        /// Attributes of foreign key.
        fk_attrs: List,
        /// Corresponding attrs in PK table.
        pk_attrs: List,
        /// FULL, PARTIAL, SIMPLE.
        fk_matchtype: u8,
        /// ON UPDATE action.
        fk_upd_action: u8,
        /// ON DELETE action.
        fk_del_action: u8,
        /// Skip validation of existing rows?
        skip_validation: bool,
        /// Mark the new constraint as valid?
        initially_valid: bool,
        /// Existing index to use; otherwise NULL.
        compression_name: Option<String>,
    }
    /// 'PGCollateClause': a COLLATE expression.
    CollateClause {
        /// Input expression.
        arg: Option<Node>,
        /// Possibly-qualified collation name.
        collname: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGTableLikeClause': CREATE TABLE ( ..
    TableLikeClause {
        /// 'relation'.
        relation: Option<Box<RangeVar>>,
        /// OR of PGTableLikeOption flags.
        options: u32,
    }
    /// 'PGTypeName': specifies a type in definitions For PGTypeName structures generated
    /// internally, it is often easier to specify the type by OID than by name.
    TypeName {
        /// Qualified name (list of PGValue strings)
        names: List,
        /// Is a set?
        setof: bool,
        /// %TYPE specified?
        pct_type: bool,
        /// Type modifier expression(s)
        typmods: List,
        /// Array bounds.
        array_bounds: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGAExpr': The 'PGAExpr' node.
    AExpr {
        /// See above.
        kind: AExprKind,
        /// Possibly-qualified name of operator.
        name: List,
        /// Left argument, or NULL if none.
        lexpr: Option<Node>,
        /// Right argument, or NULL if none.
        rexpr: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGColumnRef': specifies a reference to a column, or possibly a whole tuple The "fields"
    /// list must be nonempty.
    ColumnRef {
        /// Field names (PGValue strings) or PGAStar.
        fields: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGParamRef': specifies a $n parameter reference.
    ParamRef {
        /// The number of the parameter.
        number: i32,
        /// Token location, or -1 if unknown.
        location: i32,
        /// Optional name of the parameter.
        name: Option<String>,
    }
    /// 'PGAConst': a literal constant.
    AConst {
        /// Value (includes type info, see value.h)
        val: Value,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGIntervalConstant': Interval Constant.
    IntervalConstant {
        /// Interval constant type, either duckdb_libpgquery::T_PGString,
        /// duckdb_libpgquery::T_PGInteger or duckdb_libpgquery::T_PGAExpr.
        val_type: i32,
        /// Duckdb_libpgquery::T_PGString.
        sval: Option<String>,
        /// Duckdb_libpgquery::T_PGString.
        ival: i32,
        /// Duckdb_libpgquery::T_PGAExpr.
        eval: Option<Node>,
        /// How to interpret the interval constant (year, month, day, etc)
        typmods: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGTypeCast': a CAST expression.
    TypeCast {
        /// The expression being casted.
        arg: Option<Node>,
        /// The target type.
        type_name: Option<Box<TypeName>>,
        /// TRY_CAST or CAST.
        try_cast: i32,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGFuncCall': a function or aggregate invocation agg_order (if not NIL) indicates we saw
    /// 'foo(..
    FuncCall {
        /// Qualified name of function.
        funcname: List,
        /// The arguments (list of exprs)
        args: List,
        /// ORDER BY (list of PGSortBy)
        agg_order: List,
        /// FILTER clause, if any.
        agg_filter: Option<Node>,
        /// EXPORT_STATE clause, if any.
        export_state: bool,
        /// ORDER BY appeared in WITHIN GROUP.
        agg_within_group: bool,
        /// Argument was really '*'.
        agg_star: bool,
        /// Arguments were labeled DISTINCT.
        agg_distinct: bool,
        /// Arguments were labeled IGNORE NULLS.
        agg_ignore_nulls: IgnoreNulls,
        /// Last argument was labeled VARIADIC.
        func_variadic: bool,
        /// OVER clause, if any.
        over: Option<Box<WindowDef>>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGAStar': '*' representing all columns of a table or compound field This can appear within
    /// ColumnRef.fields, AIndirection.indirection, and ResTarget.indirection lists.
    AStar {
        /// Relation name (optional)
        relation: Option<String>,
        /// Optional: the expression (regex or list) to select columns.
        expr: Option<Node>,
        /// Optional: EXCLUDE list.
        except_list: List,
        /// Optional: REPLACE list.
        replace_list: List,
        /// Optional: RENAME list.
        rename_list: List,
        /// Whether or not this is a columns list.
        columns: bool,
        /// 'location'.
        location: i32,
    }
    /// 'PGAIndices': array subscript or slice bounds ([idx] or [lidx:uidx]) In slice case, either
    /// or both of lidx and uidx can be NULL (omitted)
    AIndices {
        /// True if slice (i.e., colon present)
        is_slice: bool,
        /// Slice lower bound, if any.
        lidx: Option<Node>,
        /// Subscript, or slice upper bound if any.
        uidx: Option<Node>,
        /// Slice step, if any.
        step: Option<Node>,
    }
    /// 'PGAIndirection': select a field and/or array element from an expression The indirection
    /// list can contain PGAIndices nodes (representing subscripting), string PGValue nodes
    /// (representing field selection --- the string value is the name of the field to select), and
    /// PGAStar nodes (representing selection of all fields of a composite type)
    AIndirection {
        /// The thing being selected from.
        arg: Option<Node>,
        /// Subscripts and/or field names and/or *.
        indirection: List,
    }
    /// 'PGBoolExpr': The 'PGBoolExpr' node.
    BoolExpr {
        /// 'boolop'.
        boolop: BoolExprType,
        /// Arguments to this expression.
        args: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGSubLink': The 'PGSubLink' node.
    SubLink {
        /// See above.
        sub_link_type: SubLinkType,
        /// Outer-query test for ALL/ANY/ROWCOMPARE.
        testexpr: Option<Node>,
        /// Originally specified operator name.
        oper_name: List,
        /// Subselect as PGQuery* or raw parsetree.
        subselect: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGCaseExpr': a CASE expression We support two distinct forms of CASE expression: CASE WHEN
    /// boolexpr THEN expr [ WHEN boolexpr THEN expr ..
    CaseExpr {
        /// Implicit equality comparison argument.
        arg: Option<Node>,
        /// The arguments (list of WHEN clauses)
        args: List,
        /// The default result (ELSE clause)
        defresult: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGCaseWhen': one arm of a CASE expression.
    CaseWhen {
        /// Condition expression.
        expr: Option<Node>,
        /// Substitution result.
        result: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGCoalesceExpr': a COALESCE expression.
    CoalesceExpr {
        /// The arguments.
        args: List,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGNullTest': The 'PGNullTest' node.
    NullTest {
        /// Input expression.
        arg: Option<Node>,
        /// IS NULL, IS NOT NULL.
        nulltesttype: NullTestType,
        /// T to perform field-by-field null checks.
        argisrow: bool,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGBooleanTest': The 'PGBooleanTest' node.
    BooleanTest {
        /// Input expression.
        arg: Option<Node>,
        /// Test type.
        booltesttype: BoolTestType,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGSetToDefault': Placeholder node for a DEFAULT marker in an INSERT or UPDATE command.
    SetToDefault {
        /// Collation for the substituted value.
        collation: u32,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGNamedArgExpr': a named argument of a function This node type can only appear in the args
    /// list of a PGFuncCall or PGFuncExpr node.
    NamedArgExpr {
        /// The argument expression.
        arg: Option<Node>,
        /// The name.
        name: Option<String>,
        /// Argument's number in positional notation.
        argnumber: i32,
        /// Argument name location, or -1 if unknown.
        location: i32,
    }
    /// 'PGGroupingFunc': A PGGroupingFunc is a GROUPING(...) expression, which behaves in many ways
    /// like an aggregate function (e.g.
    GroupingFunc {
        /// Arguments, not evaluated but kept for benefit of EXPLAIN etc.
        args: List,
        /// Actual column positions set by planner.
        cols: List,
        /// Token location.
        location: i32,
    }
    /// 'PGLambdaFunction': Lambda Function.
    LambdaFunction {
        /// Parameter list.
        lhs: List,
        /// Lambda expression.
        rhs: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGSingleArrowFunction': Single Arrow Function.
    SingleArrowFunction {
        /// 'lhs'.
        lhs: Option<Node>,
        /// 'rhs'.
        rhs: Option<Node>,
        /// Token location, or -1 if unknown.
        location: i32,
    }
    /// 'PGPositionalReference': Positional Reference.
    PositionalReference {
        /// 'position'.
        position: i32,
        /// Token location, or -1 if unknown.
        location: i32,
    }
}
