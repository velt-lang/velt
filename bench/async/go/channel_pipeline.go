// 4 producer goroutines send 250k numbers each through one buffered channel (capacity 1024) to
// a consumer that sums them. Run with `go run bench/async/go/channel_pipeline.go`.
package main

import (
	"fmt"
	"sync"
)

func main() {
	ch := make(chan int64, 1024)
	var wg sync.WaitGroup
	for id := int64(0); id < 4; id++ {
		wg.Add(1)
		go func(id int64) {
			defer wg.Done()
			for i := int64(0); i < 250000; i++ {
				ch <- id*250000 + i
			}
		}(id)
	}
	go func() {
		wg.Wait()
		close(ch)
	}()
	var total, count int64
	for v := range ch {
		total += v
		count++
	}
	fmt.Println(count, total)
}
