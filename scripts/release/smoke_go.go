package main

import (
	"database/sql"
	"fmt"
	"log"

	_ "github.com/duckdb/duckdb-go/v2"
)

func main() {
	db, err := sql.Open("duckdb", "")
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()
	var answer int
	if err := db.QueryRow("SELECT 42").Scan(&answer); err != nil {
		log.Fatal(err)
	}
	if answer != 42 {
		log.Fatalf("expected 42, got %d", answer)
	}
	fmt.Println("Go preview smoke passed")
}
