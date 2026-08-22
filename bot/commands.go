package main

import (
	"context"
	"fmt"
	"log"
	"math/rand"
	"os"
	"time"

	"github.com/disgoorg/disgo/discord"
	"github.com/disgoorg/disgo/events"
)

func RegisterCommands(clientID discord.Snowflake, rest interface {
	CreateGlobalCommands(ctx context.Context, applicationID discord.Snowflake, commands []discord.ApplicationCommandCreate, opts ...discord.RequestOpt) ([]discord.ApplicationCommand, error)
}) {
	commands := []discord.ApplicationCommandCreate{
		discord.SlashCommandCreate{
			Name:        "join",
			Description: "Join your voice channel",
		},
		discord.SlashCommandCreate{
			Name:        "leave",
			Description: "Leave the voice channel",
		},
		discord.SlashCommandCreate{
			Name:        "stop",
			Description: "Stop audio playback",
		},
		discord.SlashCommandCreate{
			Name:        "speak",
			Description: "Repeat a sentence",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionString{
					Name:        "text",
					Description: "The sentence to repeat",
					Required:    true,
				},
				discord.ApplicationCommandOptionString{
					Name:        "voice",
					Description: "The voice to use",
					Required:    false,
				},
			},
		},
		discord.SlashCommandCreate{
			Name:        "random",
			Description: "Say a random sentence from the database",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionString{
					Name:        "voice",
					Description: "The voice to use",
					Required:    false,
				},
				discord.ApplicationCommandOptionString{
					Name:        "text",
					Description: "Filter sentences by text",
					Required:    false,
				},
			},
		},
	}

	if _, err := rest.CreateGlobalCommands(context.Background(), clientID, commands); err != nil {
		log.Fatalf("Failed to register commands: %v", err)
	}
	log.Println("Commands registered")
}

func HandleCommand(e *events.ApplicationCommandInteractionCreate) {
	switch e.Data.CommandName() {
	case "join":
		handleJoin(e)
	case "leave":
		handleLeave(e)
	case "stop":
		handleStop(e)
	case "speak":
		handleSpeak(e)
	case "random":
		handleRandom(e)
	}
}

func getUserVoiceChannelID(e *events.ApplicationCommandInteractionCreate) *discord.Snowflake {
	voiceState, ok := e.Client().Cache().VoiceState(e.GuildID(), e.User().ID())
	if !ok || voiceState.ChannelID == nil {
		return nil
	}
	return voiceState.ChannelID
}

func handleJoin(e *events.ApplicationCommandInteractionCreate) {
	channelID := getUserVoiceChannelID(e)
	if channelID == nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Devi essere connesso a un canale vocale per utilizzare questo comando").SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la connessione al canale vocale").SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Sto entrando nel canale").SetEphemeral(true).Build())
}

func handleLeave(e *events.ApplicationCommandInteractionCreate) {
	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Non sono connesso a nessun canale").SetEphemeral(true).Build())
		return
	}
	voiceClient.Disconnect(context.Background())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Sto lasciando il canale").SetEphemeral(true).Build())
}

func handleStop(e *events.ApplicationCommandInteractionCreate) {
	StopAudio(e.GuildID().String())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Interrompo il bot").SetEphemeral(true).Build())
}

func handleSpeak(e *events.ApplicationCommandInteractionCreate) {
	e.DeferCreateMessage(true)

	channelID := getUserVoiceChannelID(e)
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Devi essere connesso a un canale vocale").SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la connessione al canale vocale").SetEphemeral(true).Build())
		return
	}

	text := e.Data.String("text")
	voiceName := e.Data.String("voice")
	if voiceName == "" {
		voiceName = "Google"
	}

	filePath := GetAudioFilePath(text, voiceName)
	if _, err := os.Stat(filePath); err != nil {
		var audioData []byte
		var err error
		if voiceName == "Google" {
			audioData, err = GetTTSGoogle(text)
		} else {
			audioData, err = GetTTSFakeYou(text, voiceName)
		}
		if err != nil {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore nella generazione dell'audio").SetEphemeral(true).Build())
			return
		}
		tempPath := filePath + ".tmp"
		SaveAudio(tempPath, audioData)
		CompressAudio(tempPath, filePath)
		os.Remove(tempPath)
	}

	go func() {
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
	}()

	e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(fmt.Sprintf("Sto riproducendo: %s", text)).SetEphemeral(true).Build())
}

func handleRandom(e *events.ApplicationCommandInteractionCreate) {
	e.DeferCreateMessage(true)

	channelID := getUserVoiceChannelID(e)
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Devi essere connesso a un canale vocale").SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la connessione al canale vocale").SetEphemeral(true).Build())
		return
	}

	voiceName := e.Data.String("voice")
	if voiceName == "" {
		voiceName = "Google"
	}

	text := e.Data.String("text")
	var sentences []string
	var err error
	if text != "" {
		sentences, err = db.SelectLikeSentence(text)
	} else {
		sentences, err = db.SelectAllSentence()
	}
	if err != nil || len(sentences) == 0 {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Nessuna frase trovata").SetEphemeral(true).Build())
		return
	}

	rand.Seed(time.Now().UnixNano())
	sentence := sentences[rand.Intn(len(sentences))]

	filePath := GetAudioFilePath(sentence, voiceName)
	if _, err := os.Stat(filePath); err != nil {
		var audioData []byte
		if voiceName == "Google" {
			audioData, err = GetTTSGoogle(sentence)
		} else {
			audioData, err = GetTTSFakeYou(sentence, voiceName)
		}
		if err != nil {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore nella generazione dell'audio").SetEphemeral(true).Build())
			return
		}
		tempPath := filePath + ".tmp"
		SaveAudio(tempPath, audioData)
		CompressAudio(tempPath, filePath)
		os.Remove(tempPath)
	}

	go func() {
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
	}()

	e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(fmt.Sprintf("Sto riproducendo: %s", sentence)).SetEphemeral(true).Build())
}
