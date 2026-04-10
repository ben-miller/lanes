package main

import (
	"os"

	"github.com/bmiller/spinner/cmd"
)

func main() {
	if err := cmd.Execute(); err != nil {
		os.Exit(1)
	}
}
