#!/usr/bin/env python3
"""Paired PostgreSQL mutation controls using the owned restored-database harness."""
import attach_control

attach_control.CASES = [
    ('update', "UPDATE p.source SET s=s||'-updated',matrix=[id,42] WHERE id<=3; SELECT id,s,matrix FROM p.source WHERE id<=4 ORDER BY id", None),
    ('delete', "DELETE FROM p.source WHERE id<=3; SELECT count(*),min(id) FROM p.source", None),
    ('update-from', "CREATE TABLE local_update AS SELECT 2 id,'two' s UNION ALL SELECT 7,'seven'; UPDATE p.source SET s=l.s FROM local_update l WHERE source.id=l.id; SELECT id,s FROM p.source WHERE id IN (2,7) ORDER BY id", None),
    ('delete-using', "CREATE TABLE local_delete AS SELECT 2 id UNION ALL SELECT 7; DELETE FROM p.source USING local_delete l WHERE source.id=l.id; SELECT count(*),sum(id) FROM p.source", None),
    ('correlated-update', "UPDATE p.source SET s=(SELECT 'value-'||i::VARCHAR FROM range(1,21)t(i) WHERE i=source.id) WHERE id<=3; SELECT id,s FROM p.source WHERE id<=3 ORDER BY id", None),
    ('rollback', "BEGIN; UPDATE p.source SET s='changed' WHERE id=1; DELETE FROM p.source WHERE id=2; SELECT count(*) FROM p.source; ROLLBACK; SELECT id,s FROM p.source WHERE id<=2 ORDER BY id", None),
    ('empty-update', "UPDATE p.source SET s='x' WHERE id<0; DELETE FROM p.source WHERE id<0; SELECT count(*) FROM p.source", None),
    ('default-empty', "UPDATE p.source SET s=DEFAULT WHERE id<0; SELECT count(*) FROM p.source", None),
    ('default-error', "UPDATE p.source SET s=DEFAULT WHERE id=1", 'SET DEFAULT is not yet supported'),
    ('update-returning', "UPDATE p.source SET s='x' RETURNING *", 'RETURNING clause not yet supported'),
    ('delete-returning', "DELETE FROM p.source RETURNING *", 'RETURNING clause not yet supported'),
    ('merge', "CREATE TABLE changes AS SELECT 1 id,'new' s UNION ALL SELECT 21,'inserted'; MERGE INTO p.source AS t USING changes AS s ON t.id=s.id WHEN MATCHED THEN UPDATE SET s=s.s WHEN NOT MATCHED THEN INSERT(id,s,matrix) VALUES(s.id,s.s,NULL); SELECT id,s FROM p.source WHERE id IN (1,21) ORDER BY id", None),
    ('merge-delete', "MERGE INTO p.source t USING (VALUES (1),(2),(21)) s(id) ON t.id=s.id WHEN MATCHED AND t.id=1 THEN DELETE WHEN MATCHED THEN UPDATE SET s='changed' WHEN NOT MATCHED THEN INSERT(id,s,matrix) VALUES(s.id,'new',NULL); SELECT id,s FROM p.source WHERE id<=3 OR id=21 ORDER BY id", None),
    ('merge-self', "MERGE INTO p.source t USING (SELECT id+100 id,s,matrix FROM p.source) s USING(id) WHEN NOT MATCHED THEN INSERT VALUES(s.id,s.s,s.matrix); SELECT count(*),sum(id) FROM p.source", None),
    ('merge-returning', "MERGE INTO p.source t USING (VALUES (1)) s(id) USING(id) WHEN MATCHED THEN DELETE RETURNING *", 'RETURNING is not implemented for Postgres yet'),
    ('on-conflict', "INSERT INTO p.source VALUES(1,'replaced',[42]),(21,'new',[21]) ON CONFLICT(id) DO UPDATE SET s=excluded.s,matrix=excluded.matrix; SELECT id,s,matrix FROM p.source WHERE id IN (1,21) ORDER BY id", None),
    ('drop-missing', 'DROP TABLE IF EXISTS p.absent; SELECT count(*) FROM p.source', None),
    ('alter-columns', "ALTER TABLE p.source ADD COLUMN n INTEGER DEFAULT 7; ALTER TABLE p.source RENAME COLUMN n TO renamed; SELECT sum(renamed) FROM p.source; ALTER TABLE p.source DROP COLUMN renamed; ALTER TABLE p.source RENAME TO renamed_table; SELECT count(*) FROM p.renamed_table", None),
    ('dropped-key-gap', "CREATE TABLE p.gapped(d INTEGER,e INTEGER,a INTEGER,b INTEGER,UNIQUE(a,b)); ALTER TABLE p.gapped DROP COLUMN e; INSERT INTO p.gapped VALUES(10,1,2); INSERT INTO p.gapped(a,b,d) VALUES(1,2,20) ON CONFLICT(a,b) DO UPDATE SET d=excluded.d; SELECT * FROM p.gapped", None),
    ('unsupported-alter', "ALTER TABLE p.source ALTER COLUMN s SET DEFAULT 'x'", 'Unsupported ALTER TABLE type'),
    ('large-delete', "CREATE TABLE p.large AS SELECT i::INTEGER id FROM range(20000)t(i); DELETE FROM p.large WHERE id%2=0; SELECT count(*),sum(id) FROM p.large", None),
    ('large-update', "CREATE TABLE p.large AS SELECT i::INTEGER id,0::INTEGER n FROM range(20000)t(i); UPDATE p.large SET n=id+7 WHERE id%2=0; SELECT count(*),sum(n) FROM p.large", None),
]

if __name__ == '__main__':
    attach_control.main()
