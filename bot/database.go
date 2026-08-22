package main

import (
	"database/sql"
	"log"

	_ "github.com/mattn/go-sqlite3"
)

type Database struct {
	db *sql.DB
}

func NewDatabase(dbPath string) (*Database, error) {
	db, err := sql.Open("sqlite3", dbPath)
	if err != nil {
		return nil, err
	}
	return &Database{db: db}, nil
}

func (d *Database) CreateTables() error {
	_, err := d.db.Exec(`
		CREATE TABLE IF NOT EXISTS sentences (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			sentence TEXT NOT NULL UNIQUE,
			has_audio BOOLEAN DEFAULT 0
		);
	`)
	return err
}

func (d *Database) InsertSentence(sentence string) error {
	_, err := d.db.Exec("INSERT OR IGNORE INTO sentences (sentence) VALUES (?)", sentence)
	return err
}

func (d *Database) SelectLikeSentence(text string) ([]string, error) {
	rows, err := d.db.Query("SELECT sentence FROM sentences WHERE sentence LIKE ? ORDER BY RANDOM()", "%"+text+"%")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var sentences []string
	for rows.Next() {
		var s string
		if err := rows.Scan(&s); err != nil {
			return nil, err
		}
		sentences = append(sentences, s)
	}
	return sentences, nil
}

func (d *Database) SelectAllSentence() ([]string, error) {
	rows, err := d.db.Query("SELECT sentence FROM sentences ORDER BY RANDOM()")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var sentences []string
	for rows.Next() {
		var s string
		if err := rows.Scan(&s); err != nil {
			return nil, err
		}
		sentences = append(sentences, s)
	}
	return sentences, nil
}

// SelectRandomSentenceWithoutAudio will be used by the background generator
func (d *Database) SelectRandomSentenceWithoutAudio() (string, error) {
	var sentence string
	err := d.db.QueryRow("SELECT sentence FROM sentences WHERE has_audio = 0 ORDER BY RANDOM() LIMIT 1").Scan(&sentence)
	return sentence, err
}

func (d *Database) UpdateSentenceHasAudio(sentence string) error {
	_, err := d.db.Exec("UPDATE sentences SET has_audio = 1 WHERE sentence = ?", sentence)
	return err
}
