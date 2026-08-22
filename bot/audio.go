package main

import (
	"os/exec"
	"strings"
	"sync"

	"github.com/disgoorg/disgo/voice"
	"github.com/disgoorg/ogg"
)

var (
	audioCmds   = make(map[string]*exec.Cmd)
	audioCmdsMu sync.Mutex
)

// CompressAudio will use ffmpeg to compress the mp3 to a low bitrate
func CompressAudio(inputPath, outputPath string) error {
	cmd := exec.Command("ffmpeg", "-y", "-i", inputPath, "-b:a", "32k", "-ac", "1", outputPath)
	return cmd.Run()
}

// PlayAudio starts streaming an mp3 file to a Discord voice channel.
// It converts the mp3 to opus on the fly using ffmpeg.
func PlayAudio(voiceClient voice.Client, guildID string, filePath string) error {
	StopAudio(guildID)

	cmd := exec.Command("ffmpeg", "-i", filePath, "-c:a", "libopus", "-f", "ogg", "pipe:1")
	out, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		return err
	}

	audioCmdsMu.Lock()
	audioCmds[guildID] = cmd
	audioCmdsMu.Unlock()

	oggReader := ogg.NewDecodeReader(out)
	voiceClient.SetAudioProvider(voice.NewOggOpusProvider(oggReader))

	err := cmd.Wait()
	if err != nil && !strings.Contains(err.Error(), "signal: killed") {
		return err
	}
	return nil
}

// StopAudio stops the current audio playback for a guild.
func StopAudio(guildID string) {
	audioCmdsMu.Lock()
	defer audioCmdsMu.Unlock()
	if cmd, ok := audioCmds[guildID]; ok {
		if cmd.Process != nil {
			cmd.Process.Kill()
		}
		delete(audioCmds, guildID)
	}
}
