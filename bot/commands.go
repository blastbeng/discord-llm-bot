package main

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"log"
	"math/rand"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/disgoorg/disgo/bot"
	"github.com/disgoorg/disgo/discord"
	"github.com/disgoorg/disgo/events"
	"github.com/google/uuid"
	"github.com/shirou/gopsutil/v3/cpu"
	"github.com/shirou/gopsutil/v3/mem"
)

var (
	cooldowns   = make(map[discord.Snowflake]time.Time)
	cooldownsMu sync.Mutex
)

type audioMessageInfo struct {
	Text  string
	Voice string
}

var (
	audioMessages   = make(map[string]audioMessageInfo)
	audioMessagesMu sync.Mutex
)

func checkCooldown(userID discord.Snowflake) bool {
	cooldownsMu.Lock()
	defer cooldownsMu.Unlock()
	if last, ok := cooldowns[userID]; ok {
		if time.Since(last) < 5*time.Second {
			return false
		}
	}
	cooldowns[userID] = time.Now()
	return true
}

func getQueueMessage() string {
	cpuPercent, _ := cpu.Percent(100*time.Millisecond, false)
	vmStat, _ := mem.VirtualMemory()
	swapStat, _ := mem.SwapMemory()
	ramPercent := (vmStat.UsedPercent + swapStat.UsedPercent) / 2
	cpuStr := "0.0"
	if len(cpuPercent) > 0 {
		cpuStr = fmt.Sprintf("%.1f", cpuPercent[0])
	}
	return fmt.Sprintf("\n\nSe il server é sovraccarico, potrebbe volerci un po' di tempo\n*CPU: %s%% - RAM: %.2f%%*", cpuStr, ramPercent)
}

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
					Name:         "voice",
					Description:  "The voice to use",
					Required:     false,
					Autocomplete: true,
				},
			},
		},
		discord.SlashCommandCreate{
			Name:        "random",
			Description: "Say a random sentence from the database",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionString{
					Name:         "voice",
					Description:  "The voice to use",
					Required:     false,
					Autocomplete: true,
				},
				discord.ApplicationCommandOptionString{
					Name:        "text",
					Description: "Filter sentences by text",
					Required:    false,
				},
			},
		},
		discord.SlashCommandCreate{
			Name:        "restart",
			Description: "Restart the bot",
		},
		discord.SlashCommandCreate{
			Name:        "rename",
			Description: "Rename the bot",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionString{
					Name:        "name",
					Description: "The new nickname (max 32 chars)",
					Required:    true,
				},
			},
		},
		discord.SlashCommandCreate{
			Name:        "avatar",
			Description: "Change the bot avatar",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionAttachment{
					Name:        "image",
					Description: "The new avatar image",
					Required:    true,
				},
			},
		},
		discord.SlashCommandCreate{
			Name:        "audio",
			Description: "Audio playback from the input audio",
			Options: []discord.ApplicationCommandOption{
				discord.ApplicationCommandOptionAttachment{
					Name:        "audio",
					Description: "The file audio (mp3 or wav)",
					Required:    true,
				},
			},
		},
	}

	if _, err := rest.CreateGlobalCommands(context.Background(), clientID, commands); err != nil {
		log.Fatalf("Failed to register commands: %v", err)
	}
	log.Println("Commands registered")
}

func HandleAutocomplete(e *events.AutocompleteInteractionCreate) {
	cmdName := e.Data.CommandName()
	if cmdName != "speak" && cmdName != "random" {
		return
	}

	focusedName := e.Data.FocusedName()
	if focusedName != "voice" {
		return
	}

	current := e.Data.String("voice")

	allVoices := []string{"Google", "random"}
	for voice := range fakeYouVoices {
		allVoices = append(allVoices, voice)
	}

	var choices []discord.ApplicationCommandOptionChoiceString
	for _, voice := range allVoices {
		if strings.Contains(strings.ToLower(voice), strings.ToLower(current)) {
			choices = append(choices, discord.ApplicationCommandOptionChoiceString{
				Name:  voice,
				Value: voice,
			})
		}
	}

	e.CreateAutocompleteResponse(choices)
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
	case "restart":
		handleRestart(e)
	case "rename":
		handleRename(e)
	case "avatar":
		handleAvatar(e)
	case "audio":
		handleAudio(e)
	}
}

func getVoiceChannelID(client bot.Client, guildID discord.Snowflake, userID discord.Snowflake) *discord.Snowflake {
	voiceState, ok := client.Cache().VoiceState(guildID, userID)
	if !ok || voiceState.ChannelID == nil {
		return nil
	}
	return voiceState.ChannelID
}

func handleJoin(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
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
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Non sono connesso a nessun canale").SetEphemeral(true).Build())
		return
	}
	voiceClient.Disconnect(context.Background())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Sto lasciando il canale").SetEphemeral(true).Build())
}

func handleStop(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	StopAudio(e.GuildID().String())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Interrompo il bot").SetEphemeral(true).Build())
}

func handleSpeak(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
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
	if voiceName == "" || voiceName == "random" {
		voiceName = GetRandomVoice()
	}

	filePath := GetAudioFilePath(text, voiceName)

	playID := uuid.NewString()
	audioMessagesMu.Lock()
	audioMessages[playID] = audioMessageInfo{Text: text, Voice: voiceName}
	audioMessagesMu.Unlock()

	msg, err := e.CreateFollowupMessage(discord.NewMessageCreateBuilder().
		SetContent(fmt.Sprintf("Inizio a generare l'audio per la frase: **%s**%s", text, getQueueMessage())).
		SetComponents(discord.NewActionRow(
			discord.NewPrimaryButton("Play", "play:"+playID),
			discord.NewDangerButton("Stop", "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		log.Printf("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		if _, err := os.Stat(filePath); err != nil {
			var audioData []byte
			var err error
			if voiceName == "Google" {
				audioData, err = GetTTSGoogle(text)
			} else {
				audioData, err = GetTTSFakeYou(text, voiceName)
				if err != nil {
					log.Printf("FakeYou failed, falling back to Google: %v", err)
					voiceName = "Google"
					filePath = GetAudioFilePath(text, voiceName)
					audioData, err = GetTTSGoogle(text)
					_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(fmt.Sprintf("Sto riproducendo: %s\nVoce: %s\n\nWARNING: FakeYou sta ricevendo troppe richieste, audio generato usando la voce di Google", text, voiceName)).Build())
				}
			}
			if err != nil {
				log.Printf("Error generating audio: %v", err)
				return
			}
			tempPath := filePath + ".tmp"
			if voiceName != "Google" {
				tempPath = filePath + ".wav.tmp"
			}
			if err := SaveAudio(tempPath, audioData); err != nil {
				log.Printf("Error saving audio: %v", err)
				return
			}
			if err := CompressAudio(tempPath, filePath); err != nil {
				log.Printf("Error compressing audio: %v", err)
				os.Remove(tempPath)
				return
			}
			os.Remove(tempPath)
			if voiceName == "Google" {
				db.UpdateSentenceHasAudio(text)
				db.InsertSentence(text)
			}
		}
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(fmt.Sprintf("Sto riproducendo: %s\nVoce: %s", text, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewPrimaryButton("Play", "play:"+playID),
				discord.NewDangerButton("Stop", "stop:"+e.GuildID().String()),
			)).
			Build())
	}(msg.ID)
}

func handleRandom(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
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
	if voiceName == "" || voiceName == "random" {
		voiceName = GetRandomVoice()
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

	playID := uuid.NewString()
	audioMessagesMu.Lock()
	audioMessages[playID] = audioMessageInfo{Text: sentence, Voice: voiceName}
	audioMessagesMu.Unlock()

	msg, err := e.CreateFollowupMessage(discord.NewMessageCreateBuilder().
		SetContent(fmt.Sprintf("Sto cercando una frase casuale%s", getQueueMessage())).
		SetComponents(discord.NewActionRow(
			discord.NewPrimaryButton("Play", "play:"+playID),
			discord.NewDangerButton("Stop", "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		log.Printf("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		if _, err := os.Stat(filePath); err != nil {
			var audioData []byte
			var err error
			if voiceName == "Google" {
				audioData, err = GetTTSGoogle(sentence)
			} else {
				audioData, err = GetTTSFakeYou(sentence, voiceName)
				if err != nil {
					log.Printf("FakeYou failed, falling back to Google: %v", err)
					voiceName = "Google"
					filePath = GetAudioFilePath(sentence, voiceName)
					audioData, err = GetTTSGoogle(sentence)
					_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(fmt.Sprintf("Sto riproducendo: %s\nVoce: %s\n\nWARNING: FakeYou sta ricevendo troppe richieste, audio generato usando la voce di Google", sentence, voiceName)).Build())
				}
			}
			if err != nil {
				log.Printf("Error generating audio: %v", err)
				return
			}
			tempPath := filePath + ".tmp"
			if voiceName != "Google" {
				tempPath = filePath + ".wav.tmp"
			}
			if err := SaveAudio(tempPath, audioData); err != nil {
				log.Printf("Error saving audio: %v", err)
				return
			}
			if err := CompressAudio(tempPath, filePath); err != nil {
				log.Printf("Error compressing audio: %v", err)
				os.Remove(tempPath)
				return
			}
			os.Remove(tempPath)
			if voiceName == "Google" {
				db.UpdateSentenceHasAudio(sentence)
			}
		}
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(fmt.Sprintf("Sto riproducendo: %s\nVoce: %s", sentence, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewPrimaryButton("Play", "play:"+playID),
				discord.NewDangerButton("Stop", "stop:"+e.GuildID().String()),
			)).
			Build())
	}(msg.ID)
}

func handleRestart(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	if e.GuildID().String() != os.Getenv("GUILD_ID") || e.User().ID().String() != os.Getenv("ADMIN_ID") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Non hai i permessi per utilizzare questo comando.").SetEphemeral(true).Build())
		return
	}
	if e.Member() == nil || !e.Member().Permissions.Has(discord.PermissionAdministrator) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Solo gli amministratori possono utilizzare questo comando").SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Sto riavviando il bot.").SetEphemeral(true).Build())
	go func() {
		time.Sleep(1 * time.Second)
		os.Exit(0)
	}()
}

func handleRename(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	name := e.Data.String("name")
	if len(name) > 32 {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Il mio nickname non può essere più lungo di 32 caratteri").SetEphemeral(true).Build())
		return
	}

	if err := e.Client().Rest().UpdateCurrentMember(e.GuildID(), discord.CurrentMemberUpdate{Nick: &name}); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il cambio di nickname.").SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(fmt.Sprintf("Mi hai rinominato in \"%s\"", name)).SetEphemeral(true).Build())
}

func handleAvatar(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	if e.GuildID().String() != os.Getenv("GUILD_ID") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Solo gli amministratori possono utilizzare questo comando nel server padre").SetEphemeral(true).Build())
		return
	}

	attachmentID := e.Data.Options.Attachment("image").Value
	attachment, ok := e.Data.Resolved.Attachments[attachmentID]
	if !ok {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il recupero dell'allegato.").SetEphemeral(true).Build())
		return
	}

	if !strings.HasPrefix(attachment.ContentType, "image/") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Questo tipo di file non è supportato").SetEphemeral(true).Build())
		return
	}

	resp, err := http.Get(attachment.URL)
	if err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il download dell'immagine.").SetEphemeral(true).Build())
		return
	}
	defer resp.Body.Close()

	imageData, err := io.ReadAll(resp.Body)
	if err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la lettura dell'immagine.").SetEphemeral(true).Build())
		return
	}

	avatar := discord.NewIconRaw(discord.IconType(attachment.ContentType), bytes.NewReader(imageData))
	if err := e.Client().Rest().UpdateCurrentUser(discord.UserUpdate{Avatar: &avatar}); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante l'aggiornamento dell'avatar.").SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("L'immagine è stata modificata").SetEphemeral(true).Build())
}

func handleAudio(e *events.ApplicationCommandInteractionCreate) {
	if !checkCooldown(e.User().ID()) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Spam detected. " + e.User().Mention() + " Ti sto guardando.\nCooldown: 5.0s").SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Devi essere connesso a un canale vocale").SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la connessione al canale vocale").SetEphemeral(true).Build())
		return
	}

	attachmentID := e.Data.Options.Attachment("audio").Value
	attachment, ok := e.Data.Resolved.Attachments[attachmentID]
	if !ok {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il recupero dell'allegato.").SetEphemeral(true).Build())
		return
	}

	ext := strings.ToLower(filepath.Ext(attachment.Filename))
	if ext != ".mp3" && ext != ".wav" {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("The file extension is not valid. Only mp3 or wav are allowed.").SetEphemeral(true).Build())
		return
	}

	// Download the audio file
	resp, err := http.Get(attachment.URL)
	if err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il download dell'audio.").SetEphemeral(true).Build())
		return
	}
	defer resp.Body.Close()

	tempPath := filepath.Join(os.Getenv("TMP_DIR"), "audio_"+uuid.NewString()+filepath.Ext(attachment.Filename))
	out, err := os.Create(tempPath)
	if err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante la creazione del file temporaneo.").SetEphemeral(true).Build())
		return
	}
	if _, err := io.Copy(out, resp.Body); err != nil {
		out.Close()
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Errore durante il salvataggio dell'audio.").SetEphemeral(true).Build())
		return
	}
	out.Close()

	go func() {
		if err := PlayAudio(voiceClient, e.GuildID().String(), tempPath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		os.Remove(tempPath)
	}()

	e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Done! I'm starting the audio playback!").SetEphemeral(true).Build())
}

func HandleButton(e *events.ButtonInteractionCreate) {
	customID := e.Data.CustomID()
	if strings.HasPrefix(customID, "play:") {
		id := strings.TrimPrefix(customID, "play:")
		audioMessagesMu.Lock()
		info, ok := audioMessages[id]
		audioMessagesMu.Unlock()
		if !ok {
			e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Audio non trovato.").SetEphemeral(true).Build())
			return
		}
		e.DeferCreateMessage(true)
		guildID := e.GuildID()
		voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(guildID)
		channelID := getVoiceChannelID(e.Client(), guildID, e.User().ID())
		if channelID != nil {
			voiceClient.Connect(context.Background(), *channelID, false, false)
		}
		filePath := GetAudioFilePath(info.Text, info.Voice)
		if _, err := os.Stat(filePath); err != nil {
			var audioData []byte
			var err error
			if info.Voice == "Google" {
				audioData, err = GetTTSGoogle(info.Text)
			} else {
				audioData, err = GetTTSFakeYou(info.Text, info.Voice)
				if err != nil {
					log.Printf("FakeYou failed, falling back to Google: %v", err)
					info.Voice = "Google"
					filePath = GetAudioFilePath(info.Text, info.Voice)
					audioData, err = GetTTSGoogle(info.Text)
				}
			}
			if err != nil {
				log.Printf("Error generating audio: %v", err)
				return
			}
			tempPath := filePath + ".tmp"
			if info.Voice != "Google" {
				tempPath = filePath + ".wav.tmp"
			}
			if err := SaveAudio(tempPath, audioData); err != nil {
				log.Printf("Error saving audio: %v", err)
				return
			}
			if err := CompressAudio(tempPath, filePath); err != nil {
				log.Printf("Error compressing audio: %v", err)
				os.Remove(tempPath)
				return
			}
			os.Remove(tempPath)
			if info.Voice == "Google" {
				db.UpdateSentenceHasAudio(info.Text)
			}
		}
		if err := PlayAudio(voiceClient, guildID.String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent("Riproduco l'audio.").SetEphemeral(true).Build())
	} else if strings.HasPrefix(customID, "stop:") {
		guildID := strings.TrimPrefix(customID, "stop:")
		StopAudio(guildID)
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent("Interrompo il bot.").SetEphemeral(true).Build())
	}
}
