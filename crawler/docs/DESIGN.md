# Design 

The crawler has the potential to fire up way too many tasks on a system as it bounces across the network. It would be neat if there was some sort of backpressure approach to control the resource usage. But what we probably (not verified by lookin at actual resource usage) want to control is the number of task, which isn't directly connected to the number of "discovered" peers queue'd up. A single task can dump 1000 peers in that bucket, or none.
