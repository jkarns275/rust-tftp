#!/bin/bash
let i=1
while [ $i -le 16 ]
do
time target/release/example-tftp-app -d 0 -w 1 "[::0]:4445" "[fe80::5e2b:5b9c:918b:a72a]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"

let i=1
while [ $i -le 16 ]
do
time target/release/example-tftp-app -d 1 -w 1 "[::0]:4445" "[fe80::5e2b:5b9c:918b:a72a]:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg"
let i++
done

echo "real 1000.00s"


