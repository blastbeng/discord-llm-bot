package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"log"
	"math/rand"
	"net/http"
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

	if err := db.PopulateDatabase("config/sentences.txt"); err != nil {
		log.Printf("Failed to populate database: %v", err)
	}

	// Start background generator only if SAVE_AUDIO is true
	if os.Getenv("SAVE_AUDIO") == "true" {
		go backgroundGenerator()
	} else {
		log.Println("SAVE_AUDIO is false, background generator disabled")
	}

	client, err := bot.New(os.Getenv("BOT_TOKEN"),
		bot.WithIntents(discord.IntentGuilds, discord.IntentGuildVoiceStates),
		bot.WithEventHandlers(func(e *events.Ready) {
			log.Printf("Logged in as %s", e.Client().ID())
			RegisterCommands(e.Client().ID(), e.Client().Rest())
		}, HandleCommand, HandleAutocomplete, HandleButton),
	)
	if err != nil {
		log.Fatalf("Failed to create bot: %v", err)
	}

	go changePresenceLoop(client)

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

		if len(sentence) > 200 {
			log.Printf("Background generator: skipping sentence '%s' (too long for Google TTS), marking as processed", sentence)
			db.UpdateSentenceHasAudio(sentence)
			continue
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
		db.UpdateSentenceHasAudio(sentence)
		log.Printf("Background generator: saved and compressed '%s'", sentence)
	}
}

func changePresenceLoop(client bot.Client) {
	ticker := time.NewTicker(6 * time.Hour)
	defer ticker.Stop()

	doPresence(client) // run once immediately

	for range ticker.C {
		doPresence(client)
	}
}

func doPresence(client bot.Client) {
	resp, err := http.Get("https://steamspy.com/api.php?request=top100in2weeks")
	if err != nil {
		log.Printf("Presence loop error: %v", err)
		return
	}
	defer resp.Body.Close()

	var games map[string]struct {
		Name string `json:"name"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&games); err != nil {
		log.Printf("Presence loop decode error: %v", err)
		return
	}

	var gameNames []string
	for _, g := range games {
		gameNames = append(gameNames, g.Name)
	}

	if len(gameNames) == 0 {
		return
	}

	game := gameNames[rand.Intn(len(gameNames))]

	if err := client.SetPresence(context.Background(), discord.NewPlayingActivity(game)); err != nil {
		log.Printf("Presence loop set presence error: %v", err)
	}
}
