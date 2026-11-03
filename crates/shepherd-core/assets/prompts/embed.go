// Package prompts embeds the default system prompt of each role (overridable per project).
package prompts

import "embed"

//go:embed *.md
var FS embed.FS
