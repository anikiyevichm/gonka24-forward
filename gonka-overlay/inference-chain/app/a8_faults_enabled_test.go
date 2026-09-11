//go:build a8faults

package app

import (
	"github.com/stretchr/testify/require"
	"os"
	"path/filepath"
	"testing"
)

type a8TestOptions map[string]interface{}

func (o a8TestOptions) Get(key string) interface{} { return o[key] }

func TestA8TaggedHookRequiresValidExplicitPlan(t *testing.T) {
	home := t.TempDir()
	options := a8TestOptions{"home": home}
	require.Empty(t, appendA8QueryFaultOptions(nil, options))
	require.NoError(t, os.MkdirAll(filepath.Join(home, "config"), 0700))
	file := filepath.Join(home, "config", "a8-query-faults.json")
	require.NoError(t, os.WriteFile(file, []byte(`{"unknown":true}`), 0600))
	require.Panics(t, func() { appendA8QueryFaultOptions(nil, options) })
	valid := `{"version":1,"chain_id":"test-chain","rules":[{"id":"one","deal":"gonka1y2a9p56kv044327uycmqdexl7zs82fs5ryv5le","host":"gonka1dkl4mah5erqggvhqkpc8j3qs5tyuetgdy552cp","epoch":5,"route":"/inference.inference.Query/EpochPerformanceSummaryByParticipant","from_height":100,"until_height":200,"kind":"handler_error"}]}`
	require.NoError(t, os.WriteFile(file, []byte(valid), 0600))
	require.Len(t, appendA8QueryFaultOptions(nil, options), 1)
}
