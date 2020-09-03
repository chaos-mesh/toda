// Copyright 2020 Chaos Mesh Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

package main

import (
	"fmt"
	"io/ioutil"
	"os"
	"strconv"
	"syscall"
	"time"
)

func main() {
	err := ioutil.WriteFile("/mnt/test/test", []byte("HELLO WORLD000"), 0644)
	if err != nil {
		fmt.Printf("Error: %v+", err)
		return
	}

	originalLength := len([]byte("HELLO WORLD"))

	var fVec []*os.File
	var mMap [][]byte

	for i := 0; i < 100; i++ {
		f, err := os.OpenFile("/mnt/test/test", os.O_RDWR, 0666)
		if err != nil {
			fmt.Printf("Error: %v+", err)
			return
		}

		fVec = append(fVec, f)
		data, err := syscall.Mmap(int(f.Fd()), 0, originalLength+3, syscall.PROT_READ|syscall.PROT_WRITE, syscall.MAP_SHARED)
		if err != nil {
			fmt.Printf("Error: %v+", err)
			return
		}
		mMap = append(mMap, data)
	}

	for i := 0; ; i++ {
		f := fVec[i%100]
		data := mMap[i%100]

		count := strconv.Itoa(i)
		for pos, char := range count {
			data[originalLength+pos] = byte(char)
		}

		time.Sleep(time.Second)

		buf := make([]byte, originalLength+len(count))
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
