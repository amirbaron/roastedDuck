"""Real main-thread Python signal cancellation across native execution entry points."""
import _thread
import json
from pathlib import Path
import sys
import threading
import time

import duckdb

outcomes = {}
for mode in ["text", "prepared", "relation", "ctas"]:
    con = duckdb.connect(config={"threads": 1})
    # Initialize parameter/import machinery before starting the timer: the signal
    # must arrive during query execution, rather than optional-library import.
    con.execute("select ?", [1]).fetchall()
    fired = threading.Event()

    def interrupt():
        time.sleep(0.1)
        fired.set()
        _thread.interrupt_main()

    worker = threading.Thread(target=interrupt)
    worker.start()
    start = time.perf_counter()
    try:
        if mode == "text":
            con.execute("select count(*) from range(100000000000)").fetchall()
        elif mode == "prepared":
            con.execute("select count(*) from range(?)", [100000000000]).fetchall()
        elif mode == "relation":
            con.sql("select count(*) from range(100000000000)").fetchall()
        else:
            con.sql("create table interrupted_result as select count(*) n from range(100000000000)")
        result = {"unexpected_success": True}
    except BaseException as error:
        result = {"error": type(error).__name__, "message": str(error)}
    elapsed = time.perf_counter() - start
    worker.join()
    try:
        result["reused_connection"] = con.execute("select 7").fetchone()
    except Exception as error:
        result["reuse_error"] = {"error": type(error).__name__, "message": str(error)}
    result["signal_sent"] = fired.is_set()
    outcomes[mode] = {"result": result, "elapsed_seconds": elapsed}
    con.close()

Path(sys.argv[1]).write_text(json.dumps({"outcomes": outcomes}, indent=2) + "\n")
print(json.dumps({"cases": len(outcomes)}))
