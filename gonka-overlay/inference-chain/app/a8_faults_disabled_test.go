//go:build !a8faults

package app

import "testing"

type a8ForbiddenOptions struct{}

func (a8ForbiddenOptions) Get(string) interface{} { panic("production hook must not read options") }

func TestA8ProductionHookDoesNotReadOptions(t *testing.T) {
	if result := appendA8QueryFaultOptions(nil, a8ForbiddenOptions{}); len(result) != 0 {
		t.Fatal("production hook installed a decorator")
	}
}
