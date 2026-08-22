package main

import (
	"context"
	"database/sql"
	"log"
	"os"
	"time"

	"github.com/disgoorg/disgo/bot"
	"github.com/disgoorg/disgo/discord"
	"github.com/disgoorg/disgo/events"
	"github.com/joho/godotenv"
)

var (
	db *Database
)

func main() {
	_ = godotenv.Load(".env")

	var err error
	db, err = NewDatabase("config/discord-bot.sqlite3")
	if err != nil {
		log.Fatalf("Failed to connect to database: %v", err)
	}
	defer db.db.Close()

	if err := db.CreateTables(); err != nil {
		log.Fatalf("Failed to create tables: %v", err)
	}

	// Start background generator
	go backgroundGenerator()

	client, err := bot.New(os.Getenv("BOT_TOKEN"),
		bot.WithIntents(discord.IntentGuilds, discord.IntentGuildVoiceStates),
		bot.WithEventHandlers(func(e *events.Ready) {
			log.Printf("Logged in as %s", e.Client().ID())
		}),
	)
	if err != nil {
		log.Fatalf("Failed to create bot: %v", err)
	}

	// TODO: Register commands here

	if err = client.OpenGateway(context.Background()); err != nil {
		log.Fatalf("Failed to open gateway: %v", err)
	}
	defer client.Close(context.Background())

	// Block forever
	select {}
}

func backgroundGenerator() {
	ticker := time.NewTicker(1 * time.Minute)
	defer ticker.Stop()

	for range ticker.C {
		sentence, err := db.SelectRandomSentenceWithoutAudio()
		if err != nil {
			if err != sql.ErrNoRows {
				log.Printf("Background generator error: %v", err)
			}
			continue
		}
		if sentence == "" {
			continue
		}

		filePath := GetAudioFilePath(sentence, "Google")
		if _, err := os.Stat(filePath); err == nil {
			continue // File already exists, skip
		}

		log.Printf("Background generator: processing sentence '%s'", sentence)
		audioData, err := GetTTSGoogle(sentence)
		if err != nil {
			log.Printf("Background generator: TTS error: %v", err)
			continue
		}

		tempPath := filePath + ".tmp"
		if err := SaveAudio(tempPath, audioData); err != nil {
			log.Printf("Background generator: Save error: %v", err)
			continue
		}

		if err := CompressAudio(tempPath, filePath); err != nil {
			log.Printf("Background generator: Compress error: %v", err)
			os.Remove(tempPath)
			continue
		}
		os.Remove(tempPath)
		log.Printf("Background generator: saved and compressed '%s'", sentence)
	}
}
