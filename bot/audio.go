package main

import (
	"os/exec"
)

// CompressAudio will use ffmpeg to compress the mp3 to a low bitrate
func CompressAudio(inputPath, outputPath string) error {
	cmd := exec.Command("ffmpeg", "-i", inputPath, "-b:a", "32k", "-ac", "1", outputPath)
	return cmd.Run()
}

// PlayAudio will be implemented to stream audio to Discord
func PlayAudio(filePath string) error {
	// Placeholder for Disgo audio playback
	return nil
}
