package main

import (
	"context"
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
			log.Printf("Background generator error: %v", err)
			continue
		}
		if sentence == "" {
			continue
		}
		log.Printf("Background generator: processing sentence '%s'", sentence)
		// TODO: Generate and save compressed audio
	}
}
