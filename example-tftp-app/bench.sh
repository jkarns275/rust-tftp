#!/bin/bash
let i=1
while [ $i -le 16 ]
do
time cargo run --release -- -d 0 -w 16 4445 "127.0.0.1:4444" "http://i0.kym-cdn.com/entries/icons/original/000/019/472/i09FJm4.jpg" | grep -Po -a "real"
let i++
done
