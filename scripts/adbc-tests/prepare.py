"""Keep all upstream test cases/assertions; replace only the two private C++ helper bodies."""
import hashlib,pathlib,re,sys
source=pathlib.Path(sys.argv[1]);out=pathlib.Path(sys.argv[2]);text=source.read_text();original_cases=re.findall(r'^TEST_CASE\(',text,re.M);assert len(original_cases)==64
assertion_count=len(re.findall(r"\b(?:REQUIRE|CHECK)(?:_[A-Z]+)*\s*\(",text))
start=text.index('\tbool QueryAndCheck(const string &query) {');end=text.index('\n\tArrowArrayStream &QueryArrow(',start)
text=text[:start]+'''\tbool QueryAndCheck(const string &query) {
\t\tQueryArrow(query);
\t\treturn NativeArrowComparison(&adbc_database,&adbc_connection,query,arrow_stream);
\t}

\tunique_ptr<MaterializedQueryResult> Query(const string &query) {
\t\treturn NativeQuery(&adbc_connection,query);
\t}
'''+text[end:]
old='\tauto conn_wrapper = static_cast<DuckDBAdbcConnectionWrapper *>(adbc_connection.private_data);\n\tauto cconn = reinterpret_cast<Connection *>(conn_wrapper->connection);\n\n\tREQUIRE(!cconn->IsAutoCommit());'
assert old in text
text=text.replace(old,'\tREQUIRE(!NativeIsAutoCommit(&adbc_connection));')
assert len(re.findall(r'^TEST_CASE\(',text,re.M))==64
text=text.replace("InitializeADBCError", "HarnessInitializeADBCError")
assert len(re.findall(r"\b(?:REQUIRE|CHECK)(?:_[A-Z]+)*\s*\(",text))==assertion_count
if not out.exists() or out.read_text()!=text:out.write_text(text)
print('upstream_sha256='+hashlib.sha256(source.read_bytes()).hexdigest())
print('adapted_sha256='+hashlib.sha256(out.read_bytes()).hexdigest())
print('test_cases=64; all case assertions retained; two private helper bodies and one private autocommit read replaced')
