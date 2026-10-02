#include <Arduino.h>
#include <NewPing.h>
#include <Keypad.h>
#include <Encoder.h>
#include "fixture-pins.h"

char keys[4][4]={{'1','2','3','A'},{'4','5','6','B'},{'7','8','9','C'},{'*','0','#','D'}};
byte rows[4]={FIXTURE_P0,FIXTURE_P1,FIXTURE_P2,FIXTURE_P3};
byte columns[4]={FIXTURE_P4,FIXTURE_P5,FIXTURE_P6,FIXTURE_P7};
Keypad keypad(makeKeymap(keys),rows,columns,4,4);
NewPing* sonar[2]={nullptr,nullptr};
Encoder* encoders[2]={nullptr,nullptr};
char mode=0;
void setup(){Serial0.begin(115200);Serial0.println("INPUT:READY");}
void loop(){
  if(Serial0.available()){
    const char command=Serial0.read();
    if(command=='U' && !mode){
      sonar[0]=new NewPing(FIXTURE_P0,FIXTURE_P1,400);
      sonar[1]=new NewPing(FIXTURE_P2,FIXTURE_P3,400);
      mode='U';Serial0.println("INPUT:ULTRASONIC");
    } else if(command=='K' && !mode){mode='K';Serial0.println("INPUT:KEYPAD");}
    else if(command=='E' && !mode){
      encoders[0]=new Encoder(FIXTURE_P0,FIXTURE_P1);
      encoders[1]=new Encoder(FIXTURE_P2,FIXTURE_P3);
      mode='E';Serial0.println("INPUT:ENCODER");
    } else if(command=='P' && mode=='U'){
      digitalWrite(FIXTURE_P0,LOW);delayMicroseconds(2);
      digitalWrite(FIXTURE_P0,HIGH);delayMicroseconds(10);digitalWrite(FIXTURE_P0,LOW);
      Serial0.printf("INPUT:PULSE:%lu\n",pulseIn(FIXTURE_P1,HIGH,30000));
    } else if(command=='N' && mode=='U'){
      const unsigned first=sonar[0]->ping_cm();delay(60);
      const unsigned second=sonar[1]->ping_cm();
      Serial0.printf("INPUT:PING:%u:%u\n",first,second);
    } else if(command=='R' && mode=='E'){
      Serial0.printf("INPUT:POSITION:%ld:%ld\n",encoders[0]->read(),encoders[1]->read());
    }
  }
  if(mode=='K'){const char key=keypad.getKey();if(key)Serial0.printf("INPUT:KEY:%c\n",key);}
  delay(1);
}
