// a8-query-fault-plan validates and fingerprints a test-only node fault plan.
// It never installs a plan or connects to a node.
package main

import (
	"encoding/json"
	"fmt"
	"github.com/productscience/inference/app/a8faults"
	"os"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: a8-query-fault-plan <plan.json>")
		os.Exit(2)
	}
	f, err := os.Open(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer f.Close()
	p, digest, err := a8faults.Load(f)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if err = json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"plan": p, "plan_sha256": digest, "scope": "A8 test-only; not acceptance evidence"}); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
