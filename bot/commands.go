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

func getOrGenerateAudio(text string, voiceName string) (filePath string, finalVoice string, err error) {
	filePath = GetAudioFilePath(text, voiceName)
	if _, err := os.Stat(filePath); err == nil {
		return filePath, voiceName, nil
	}

	var audioData []byte
	if voiceName == "Google" {
		if len(text) > 200 {
			return "", voiceName, fmt.Errorf("text too long")
		}
		audioData, err = GetTTSGoogle(text)
	} else {
		audioData, err = GetTTSFakeYou(text, voiceName)
		if err != nil {
			log.Printf("FakeYou failed, falling back to Google: %v", err)
			voiceName = "Google"
			filePath = GetAudioFilePath(text, voiceName)
			if _, err := os.Stat(filePath); err == nil {
				return filePath, voiceName, nil
			}
			if len(text) > 200 {
				return "", voiceName, fmt.Errorf("text too long")
			}
			audioData, err = GetTTSGoogle(text)
		}
	}
	if err != nil {
		return "", voiceName, err
	}

	tempPath := filePath + ".tmp"
	if voiceName != "Google" {
		tempPath = filePath + ".wav.tmp"
	}
	if err := SaveAudio(tempPath, audioData); err != nil {
		return "", voiceName, err
	}
	if err := CompressAudio(tempPath, filePath); err != nil {
		os.Remove(tempPath)
		return "", voiceName, err
	}
	os.Remove(tempPath)

	if voiceName == "Google" {
		db.InsertSentence(text)
		db.UpdateSentenceHasAudio(text)
	}
	return filePath, voiceName, nil
}

func checkCooldown(userID discord.Snowflake, mention string, commandName string) string {
	cooldownsMu.Lock()
	defer cooldownsMu.Unlock()
	if last, ok := cooldowns[userID]; ok {
		if time.Since(last) < 5*time.Second {
			remaining := 5 - time.Since(last).Seconds()
			cooldownStr := fmt.Sprintf("%s -> Cooldown: 5.0[%.2f]s", commandName, remaining)
			msgs := []string{
				T("spam_detected_1", mention, cooldownStr),
				T("spam_detected_2", mention, cooldownStr),
				T("spam_detected_3", mention, cooldownStr),
				T("spam_detected_4", mention, cooldownStr),
				T("spam_detected_5", mention, cooldownStr),
				T("spam_detected_6", mention, cooldownStr),
				T("spam_detected_7", mention, cooldownStr),
				T("spam_detected_8", mention, cooldownStr),
			}
			return msgs[rand.Intn(len(msgs))]
		}
	}
	cooldowns[userID] = time.Now()
	return ""
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
	return T("queue_message", cpuStr, ramPercent)
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
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
	if channelID == nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("must_be_in_voice")).SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("joining_channel")).SetEphemeral(true).Build())
}

func handleLeave(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("not_connected")).SetEphemeral(true).Build())
		return
	}
	StopAudio(e.GuildID().String())
	voiceClient.Disconnect(context.Background())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("leaving_channel")).SetEphemeral(true).Build())
}

func handleStop(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	StopAudio(e.GuildID().String())
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("stopping_bot")).SetEphemeral(true).Build())
}

func handleSpeak(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("must_be_in_voice")).SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
		return
	}

	text := e.Data.String("text")
	voiceName := e.Data.String("voice")
	if voiceName == "" || voiceName == "random" {
		voiceName = GetRandomVoice()
	}

	playID := uuid.NewString()
	audioMessagesMu.Lock()
	audioMessages[playID] = audioMessageInfo{Text: text, Voice: voiceName}
	audioMessagesMu.Unlock()

	msg, err := e.CreateFollowupMessage(discord.NewMessageCreateBuilder().
		SetContent(T("generating_audio", text, getQueueMessage())).
		SetComponents(discord.NewActionRow(
			discord.NewPrimaryButton(T("button_play"), "play:"+playID),
			discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		log.Printf("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		filePath, finalVoice, err := getOrGenerateAudio(text, voiceName)
		if finalVoice != voiceName {
			voiceName = finalVoice
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(T("fakeyou_fallback", text, voiceName)).Build())
		}
		if err != nil {
			log.Printf("Error generating audio: %v", err)
			errMsg := T("error_audio_generation_retry")
			if err.Error() == "text too long" {
				errMsg = T("error_text_too_long")
			}
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(text + "\nVoce: " + voiceName + errMsg).Build())
			return
		}
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(T("playing_audio", text, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewPrimaryButton(T("button_play"), "play:"+playID),
				discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
			)).
			Build())
	}(msg.ID)
}

func handleRandom(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("must_be_in_voice")).SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
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
		if text != "" {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("no_sentence_found_with_text", text)).SetEphemeral(true).Build())
		} else {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("no_sentence_found")).SetEphemeral(true).Build())
		}
		return
	}

	sentence := sentences[rand.Intn(len(sentences))]

	playID := uuid.NewString()
	audioMessagesMu.Lock()
	audioMessages[playID] = audioMessageInfo{Text: sentence, Voice: voiceName}
	audioMessagesMu.Unlock()

	msg, err := e.CreateFollowupMessage(discord.NewMessageCreateBuilder().
		SetContent(T("searching_random", getQueueMessage())).
		SetComponents(discord.NewActionRow(
			discord.NewPrimaryButton(T("button_play"), "play:"+playID),
			discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		log.Printf("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		filePath, finalVoice, err := getOrGenerateAudio(sentence, voiceName)
		if finalVoice != voiceName {
			voiceName = finalVoice
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(T("fakeyou_fallback", sentence, voiceName)).Build())
		}
		if err != nil {
			log.Printf("Error generating audio: %v", err)
			errMsg := T("error_audio_generation_retry")
			if err.Error() == "text too long" {
				errMsg = T("error_text_too_long")
			}
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(sentence + "\nVoce: " + voiceName + errMsg).Build())
			return
		}
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(T("playing_audio", sentence, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewPrimaryButton(T("button_play"), "play:"+playID),
				discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
			)).
			Build())
	}(msg.ID)
}

func handleRestart(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	if e.GuildID().String() != os.Getenv("GUILD_ID") || e.User().ID().String() != os.Getenv("ADMIN_ID") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("no_permissions")).SetEphemeral(true).Build())
		return
	}
	if e.Member() == nil || !e.Member().Permissions.Has(discord.PermissionAdministrator) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only")).SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("restarting_bot")).SetEphemeral(true).Build())
	go func() {
		time.Sleep(1 * time.Second)
		os.Exit(0)
	}()
}

func handleRename(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	if e.Member() == nil || !e.Member().Permissions.Has(discord.PermissionAdministrator) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only")).SetEphemeral(true).Build())
		return
	}
	name := e.Data.String("name")
	if len(name) > 32 {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("nick_too_long")).SetEphemeral(true).Build())
		return
	}

	if err := e.Client().Rest().UpdateCurrentMember(e.GuildID(), discord.CurrentMemberUpdate{Nick: &name}); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_nick")).SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("nick_changed", name)).SetEphemeral(true).Build())
}

func handleAvatar(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	if e.GuildID().String() != os.Getenv("GUILD_ID") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only_parent")).SetEphemeral(true).Build())
		return
	}

	attachmentID := e.Data.Options.Attachment("image").Value
	attachment, ok := e.Data.Resolved.Attachments[attachmentID]
	if !ok {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_attachment")).SetEphemeral(true).Build())
		return
	}

	if !strings.HasPrefix(attachment.ContentType, "image/") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("unsupported_file")).SetEphemeral(true).Build())
		return
	}

	resp, err := http.Get(attachment.URL)
	if err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_download_image")).SetEphemeral(true).Build())
		return
	}
	defer resp.Body.Close()

	imageData, err := io.ReadAll(resp.Body)
	if err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_read_image")).SetEphemeral(true).Build())
		return
	}

	avatar := discord.NewIconRaw(discord.IconType(attachment.ContentType), bytes.NewReader(imageData))
	if err := e.Client().Rest().UpdateCurrentUser(discord.UserUpdate{Avatar: &avatar}); err != nil {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_update_avatar")).SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("avatar_changed")).SetEphemeral(true).Build())
}

func handleAudio(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID := getVoiceChannelID(e.Client(), e.GuildID(), e.User().ID())
	if channelID == nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("must_be_in_voice")).SetEphemeral(true).Build())
		return
	}

	voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
	if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
		return
	}

	attachmentID := e.Data.Options.Attachment("audio").Value
	attachment, ok := e.Data.Resolved.Attachments[attachmentID]
	if !ok {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_attachment")).SetEphemeral(true).Build())
		return
	}

	ext := strings.ToLower(filepath.Ext(attachment.Filename))
	if ext != ".mp3" && ext != ".wav" {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("invalid_audio_ext")).SetEphemeral(true).Build())
		return
	}

	// Download the audio file
	resp, err := http.Get(attachment.URL)
	if err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_download_audio")).SetEphemeral(true).Build())
		return
	}
	defer resp.Body.Close()

	tmpDir := os.Getenv("TMP_DIR")
	if tmpDir == "" {
		tmpDir = os.TempDir()
	}
	if err := os.MkdirAll(tmpDir, 0755); err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_create_temp")).SetEphemeral(true).Build())
		return
	}
	tempPath := filepath.Join(tmpDir, "audio_"+uuid.NewString()+filepath.Ext(attachment.Filename))
	out, err := os.Create(tempPath)
	if err != nil {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_create_temp")).SetEphemeral(true).Build())
		return
	}
	if _, err := io.Copy(out, resp.Body); err != nil {
		out.Close()
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_save_audio")).SetEphemeral(true).Build())
		return
	}
	out.Close()

	go func() {
		if err := PlayAudio(voiceClient, e.GuildID().String(), tempPath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		os.Remove(tempPath)
	}()

	e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("audio_playback_started")).SetEphemeral(true).Build())
}

func HandleButton(e *events.ButtonInteractionCreate) {
	customID := e.Data.CustomID()
	if strings.HasPrefix(customID, "play:") {
		id := strings.TrimPrefix(customID, "play:")
		audioMessagesMu.Lock()
		info, ok := audioMessages[id]
		audioMessagesMu.Unlock()
		if !ok {
			e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("audio_not_found")).SetEphemeral(true).Build())
			return
		}
		e.DeferCreateMessage(true)
		guildID := e.GuildID()
		voiceClient, _ := e.Client().Voice().GetOrCreateGuildVoiceClient(guildID)
		channelID := getVoiceChannelID(e.Client(), guildID, e.User().ID())
		if channelID != nil {
			voiceClient.Connect(context.Background(), *channelID, false, false)
		}
		filePath, finalVoice, err := getOrGenerateAudio(info.Text, info.Voice)
		if finalVoice != info.Voice {
			info.Voice = finalVoice
		}
		if err != nil {
			log.Printf("Error generating audio: %v", err)
			_ = e.UpdateMessage(discord.NewMessageUpdateBuilder().SetContent(info.Text + "\nVoce: " + info.Voice + T("error_audio_generation_retry")).Build())
			return
		}
		if err := PlayAudio(voiceClient, guildID.String(), filePath); err != nil {
			log.Printf("Error playing audio: %v", err)
		}
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("playing_audio_button")).SetEphemeral(true).Build())
	} else if strings.HasPrefix(customID, "stop:") {
		guildID := strings.TrimPrefix(customID, "stop:")
		StopAudio(guildID)
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("stopping_bot_button")).SetEphemeral(true).Build())
	}
}
