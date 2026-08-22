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
	"github.com/disgoorg/disgo/rest"
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
	saveAudio := os.Getenv("SAVE_AUDIO") == "true"

	if saveAudio {
		filePath = GetAudioFilePath(text, voiceName)
		if _, err := os.Stat(filePath); err == nil {
			return filePath, voiceName, nil
		}
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
			if saveAudio {
				filePath = GetAudioFilePath(text, voiceName)
				if _, err := os.Stat(filePath); err == nil {
					return filePath, voiceName, nil
				}
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

	if saveAudio {
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

		db.InsertSentence(text)
		db.UpdateSentenceHasAudio(text)
	} else {
		// Save to a temporary file to be played
		ext := ".mp3"
		if voiceName != "Google" {
			ext = ".wav"
		}
		tempPath := filepath.Join(os.TempDir(), "audio_"+uuid.NewString()+ext)
		if err := SaveAudio(tempPath, audioData); err != nil {
			return "", voiceName, err
		}
		filePath = tempPath
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
	for id, t := range cooldowns {
		if time.Since(t) > 10*time.Second {
			delete(cooldowns, id)
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
	CreateGlobalCommands(ctx context.Context, applicationID discord.Snowflake, commands []discord.ApplicationCommandCreate, opts ...rest.RequestOpt) ([]discord.ApplicationCommand, error)
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
	LogInfo("Commands registered")
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
		if len(choices) >= 25 {
			break
		}
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
	LogDebug("Command %s executed by %s in guild %s", e.Data.CommandName, e.User().ID(), e.GuildID())
	switch e.Data.CommandName {
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

// checkVoicePermissions verifies that the user is in a voice channel and that the bot
// has Speak permission in that channel. It returns the channel ID and an error message
// (empty string if all checks pass).
func checkVoicePermissions(client bot.Client, guildID discord.Snowflake, userID discord.Snowflake) (*discord.Snowflake, string) {
	channelID := getVoiceChannelID(client, guildID, userID)
	if channelID == nil {
		return nil, T("must_be_in_voice")
	}

	guild := client.Cache().Guild(guildID)
	if guild == nil {
		return channelID, ""
	}
	channel := client.Cache().Channel(*channelID)
	if channel == nil {
		return channelID, ""
	}
	selfMember := client.Cache().Member(guildID, client.ID())
	if selfMember == nil {
		return channelID, ""
	}
	perms := discord.CalcOverwrites(guild, channel, selfMember)
	if !perms.Has(discord.PermissionSpeak) {
		return nil, T("no_permissions_channel")
	}

	return channelID, ""
}

func handleJoin(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	channelID, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID())
	if errMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
		return
	}

	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if ok && voiceClient.Connected() {
		currentChannelID, _ := voiceClient.ChannelID()
		if currentChannelID != nil && *currentChannelID == *channelID {
			e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("already_connected")).SetEphemeral(true).Build())
			return
		}
	}

	voiceClient, _ = e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
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
	if _, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID()); errMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
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
	if _, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID()); errMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
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

	channelID, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID())
	if errMsg != "" {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
		return
	}

	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		voiceClient, _ = e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
		if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
			return
		}
	} else {
		currentChannelID, _ := voiceClient.ChannelID()
		if currentChannelID == nil || *currentChannelID != *channelID {
			if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
				e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
				return
			}
		}
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
			discord.NewSuccessButton(T("button_play"), "play:"+playID),
			discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		LogError("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		defer func() {
			audioMessagesMu.Lock()
			delete(audioMessages, playID)
			audioMessagesMu.Unlock()
		}()
		filePath, finalVoice, err := getOrGenerateAudio(text, voiceName)
		if finalVoice != voiceName {
			voiceName = finalVoice
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(T("fakeyou_fallback", text, voiceName)).Build())
		}
		if err != nil {
			LogError("Error generating audio: %v", err)
			errMsg := T("error_audio_generation_retry")
			if err.Error() == "text too long" {
				errMsg = T("error_text_too_long")
			}
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(fmt.Sprintf("%s\nVoce: %s\n%s", text, voiceName, errMsg)).Build())
			return
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(T("playing_audio", text, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewSuccessButton(T("button_play"), "play:"+playID),
				discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
			)).
			Build())
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			LogError("Error playing audio: %v", err)
		}
		if os.Getenv("SAVE_AUDIO") != "true" {
			os.Remove(filePath)
		}
	}(msg.ID)
}

func handleRandom(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	e.DeferCreateMessage(true)

	channelID, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID())
	if errMsg != "" {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
		return
	}

	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		voiceClient, _ = e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
		if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
			return
		}
	} else {
		currentChannelID, _ := voiceClient.ChannelID()
		if currentChannelID == nil || *currentChannelID != *channelID {
			if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
				e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
				return
			}
		}
	}

	voiceName := e.Data.String("voice")
	if voiceName == "" || voiceName == "random" {
		voiceName = GetRandomVoice()
	}

	text := e.Data.String("text")
	sentence, err := db.SelectRandomSentence(text)
	if err != nil || sentence == "" {
		if text != "" {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("no_sentence_found_with_text", text)).SetEphemeral(true).Build())
		} else {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("no_sentence_found")).SetEphemeral(true).Build())
		}
		return
	}

	playID := uuid.NewString()
	audioMessagesMu.Lock()
	audioMessages[playID] = audioMessageInfo{Text: sentence, Voice: voiceName}
	audioMessagesMu.Unlock()

	msg, err := e.CreateFollowupMessage(discord.NewMessageCreateBuilder().
		SetContent(T("searching_random", getQueueMessage())).
		SetComponents(discord.NewActionRow(
			discord.NewSuccessButton(T("button_play"), "play:"+playID),
			discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
		)).
		SetEphemeral(true).Build())
	if err != nil {
		LogError("Error creating followup message: %v", err)
		return
	}

	go func(messageID discord.Snowflake) {
		defer func() {
			audioMessagesMu.Lock()
			delete(audioMessages, playID)
			audioMessagesMu.Unlock()
		}()
		filePath, finalVoice, err := getOrGenerateAudio(sentence, voiceName)
		if finalVoice != voiceName {
			voiceName = finalVoice
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(T("fakeyou_fallback", sentence, voiceName)).Build())
		}
		if err != nil {
			LogError("Error generating audio: %v", err)
			errMsg := T("error_audio_generation_retry")
			if err.Error() == "text too long" {
				errMsg = T("error_text_too_long")
			}
			_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().SetContent(fmt.Sprintf("%s\nVoce: %s\n%s", sentence, voiceName, errMsg)).Build())
			return
		}
		_, _ = e.Client().Rest().UpdateFollowupMessage(e.ApplicationID(), e.Token(), messageID, discord.NewMessageUpdateBuilder().
			SetContent(T("playing_audio", sentence, voiceName)).
			SetComponents(discord.NewActionRow(
				discord.NewSuccessButton(T("button_play"), "play:"+playID),
				discord.NewDangerButton(T("button_stop"), "stop:"+e.GuildID().String()),
			)).
			Build())
		if err := PlayAudio(voiceClient, e.GuildID().String(), filePath); err != nil {
			LogError("Error playing audio: %v", err)
		}
		if os.Getenv("SAVE_AUDIO") != "true" {
			os.Remove(filePath)
		}
	}(msg.ID)
}

func handleRestart(e *events.ApplicationCommandInteractionCreate) {
	if spamMsg := checkCooldown(e.User().ID(), e.User().Mention(), e.Data.CommandName()); spamMsg != "" {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(spamMsg).SetEphemeral(true).Build())
		return
	}
	if e.GuildID().String() != os.Getenv("GUILD_ID") || e.User().ID().String() != os.Getenv("ADMIN_ID") {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only_parent")).SetEphemeral(true).Build())
		return
	}
	if e.Member() == nil || !e.Member().Permissions.Has(discord.PermissionAdministrator) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only")).SetEphemeral(true).Build())
		return
	}
	e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("restarting_bot")).SetEphemeral(true).Build())
	go func() {
		time.Sleep(1 * time.Second)
		db.db.Close()
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

	if e.User().ID().String() != os.Getenv("ADMIN_ID") && (e.Member() == nil || !e.Member().Permissions.Has(discord.PermissionAdministrator)) {
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("admin_only")).SetEphemeral(true).Build())
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

	if attachment.Size > 8*1024*1024 { // 8MB limit
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("error_file_too_large")).SetEphemeral(true).Build())
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

	channelID, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID())
	if errMsg != "" {
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
		return
	}

	voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(e.GuildID())
	if !ok || !voiceClient.Connected() {
		voiceClient, _ = e.Client().Voice().GetOrCreateGuildVoiceClient(e.GuildID())
		if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
			return
		}
	} else {
		currentChannelID, _ := voiceClient.ChannelID()
		if currentChannelID == nil || *currentChannelID != *channelID {
			if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
				e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
				return
			}
		}
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

	if attachment.Size > 10*1024*1024 { // 10MB limit
		e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_file_too_large")).SetEphemeral(true).Build())
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
			LogError("Error playing audio: %v", err)
		}
		os.Remove(tempPath)
	}()

	e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("audio_playback_started")).SetEphemeral(true).Build())
}

func HandleButton(e *events.ComponentInteractionCreate) {
	customID := e.Data.CustomID()
	LogDebug("Button %s clicked by %s in guild %s", customID, e.User().ID(), e.GuildID())
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
		channelID, errMsg := checkVoicePermissions(e.Client(), guildID, e.User().ID())
		if errMsg != "" {
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
			return
		}
		voiceClient, ok := e.Client().Voice().GetGuildVoiceClient(guildID)
		if !ok || !voiceClient.Connected() {
			voiceClient, _ = e.Client().Voice().GetOrCreateGuildVoiceClient(guildID)
			if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
				e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
				return
			}
		} else {
			currentChannelID, _ := voiceClient.ChannelID()
			if currentChannelID == nil || *currentChannelID != *channelID {
				if err := voiceClient.Connect(context.Background(), *channelID, false, false); err != nil {
					e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("error_connecting")).SetEphemeral(true).Build())
					return
				}
			}
		}
		go func() {
			filePath, finalVoice, err := getOrGenerateAudio(info.Text, info.Voice)
			if finalVoice != info.Voice {
				info.Voice = finalVoice
			}
			if err != nil {
				LogError("Error generating audio: %v", err)
				_ = e.UpdateMessage(discord.NewMessageUpdateBuilder().SetContent(fmt.Sprintf("%s\nVoce: %s%s", info.Text, info.Voice, T("error_audio_generation_retry"))).Build())
				return
			}
			e.CreateFollowupMessage(discord.NewMessageCreateBuilder().SetContent(T("playing_audio_button")).SetEphemeral(true).Build())
			if err := PlayAudio(voiceClient, guildID.String(), filePath); err != nil {
				LogError("Error playing audio: %v", err)
			}
			if os.Getenv("SAVE_AUDIO") != "true" {
				os.Remove(filePath)
			}
		}()
	} else if strings.HasPrefix(customID, "stop:") {
		guildID := strings.TrimPrefix(customID, "stop:")
		if _, errMsg := checkVoicePermissions(e.Client(), e.GuildID(), e.User().ID()); errMsg != "" {
			e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(errMsg).SetEphemeral(true).Build())
			return
		}
		StopAudio(guildID)
		e.CreateMessage(discord.NewMessageCreateBuilder().SetContent(T("stopping_bot_button")).SetEphemeral(true).Build())
	}
}
