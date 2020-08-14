package main

import (
	"fmt"
	"io/ioutil"
	"os"
	"time"
)

func main() {
	err := ioutil.WriteFile("/mnt/test/test", []byte("HELLO WORLD"), 0644)
	if err != nil {
		fmt.Printf("Error: %v+", err)
		return
	}

	f, err := os.Open("/mnt/test/test")
	if err != nil {
		fmt.Printf("Error: %v+", err)
		return
	}

	for {
		time.Sleep(time.Second)

		buf := make([]byte, 5)
		n, err := f.Read(buf)
		if err != nil {
			fmt.Printf("Error: %v\n", err)
			continue
		}
		fmt.Printf("%v %d bytes: %s\n", time.Now(), n, string(buf[:n]))

		_, err = f.Seek(0, 0)
		if err != nil {
			fmt.Printf("Error: %v\n", err)
			continue
		}
	}
}
